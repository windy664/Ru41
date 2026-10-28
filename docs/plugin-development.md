# 动态插件开发指南（cdylib 热插拔）

ru41 支持 Pumpkin/bevy 式动态插件：独立 crate 编译成 `.so`，扔进插件目录，
宿主启动时加载注册——**不碰主仓库、不重编主二进制**即可上线新附属。

适合：模块多团队并行开发、第三方贡献、快速试错。
不适合替代容器隔离：插件与宿主同进程，panic 可带崩宿主。

## 边界架构（ru41-plugin-api）

宿主↔插件之间允许跨边界的类型**全部**定义在 `crates/ru41-plugin-api`：
`ModuleDef`/`PluginEntry`/`bridge`、`ports`（TaskStore/CommandHandler 等）、
`domain`（Task/NotifyRoute 等）、`Config` 整棵树。宿主在原路径
re-export（`ru41::plugins::*`、`ru41::modules::*`、`ru41::ports::*` 等），
插件照常 `ru41 = { path = ... }` 依赖即可，拿到的是同一份定义。

判别规则（新代码归哪边）：

- **跨边界**（出现在 ModuleDef 函数签名、trait 对象方法、或宿主分配的
  Config 字段里）→ 必须进 api crate，变更要 bump 版本。
- **不跨边界**（只是插件内部复用的便利代码，如 adapters/util/业务工具）
  → 留在宿主，插件经 `ru41` 依赖照常使用；宿主改这些**不影响**插件。
- 拿不准就问一句："这个类型的值会在宿主和插件之间传递吗？"会 → api；
  不会 → 宿主。

## 快速开始

```bash
cp -r templates/plugin-hello plugins/ru41-plugin-foo   # 或 plugins/ 下新目录
cd plugins/foo
# 编辑 Cargo.toml 的 name 与 src/lib.rs 的模块名

# 插件必须在与运行时镜像相同 glibc 的环境里编译（本机 glibc 比容器新会
# dlopen 失败：version `GLIBC_X.XX' not found）。用净室容器编译：
docker run --rm -v <ru41仓库路径>:/build \
  -e CARGO_TARGET_DIR=/build/plugins/foo/target-docker \
  -w /build/plugins/foo rust:1.98-trixie \
  cargo build --release --features plugin-entry

cp target-docker/release/libru41_plugin_foo.so <服务器>:/etc/ru41/plugins/
# 重启需要它的容器（poll/run 类重启模块容器；QQ 命令类重启 qq 容器）
```

验证：`ru41 schedule list` 出现模块名；`ru41 <name> poll` 输出 JSON。

## 握手（加载时校验，任一不匹配拒绝加载并给出原因）

1. **magic + 入口结构版本**：防误载/协议变更。
2. **ru41-plugin-api crate 版本一致**：边界闭包全部住在该 crate，版本一致
   即布局一致。改 api crate 任何公开项 → bump `crates/ru41-plugin-api/
   Cargo.toml` 的 version → 全部插件重编。**宿主业务代码（app/adapters/
   ingress/main）变更不需要重编插件，旧 .so 照常加载。**
3. **deps stamp 一致**：tokio/serde/chrono 等 ABI 相关外部依赖在 Cargo.lock
   中解析版本的摘要（api crate build.rs 自动生成，从仓库根 lock 提取）。
   各插件的 `Cargo.lock` 必须与根同步（deploy 脚本已自动 cp）。

`git commit` 只作诊断日志，不再是加载门槛。仍需**同一 rustc**
（Rust 无稳定 ABI；容器构建统一 rust:1.98-trixie）。

## 硬约束（违反 = ABI 未定义行为，不是报错）

0. **async 边界必须过桥**：dylib 各静态链接一份 tokio，宿主 reactor 对插件
   代码不可见——插件 future 若被宿主直接驱动，reqwest/tokio 调用会
   "no reactor running" panic 并 abort 宿主（2026-10-27 实测）。
   `PollJob::poll_json`、QQ handler 等返回 future 的入口一律：
   ```rust
   fn poll_json(&self) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>> {
       let owned = self.clone(); // bridge 要 'static
       ru41::plugins::bridge(async move { owned.poll_once().await })
   }
   ```
   桥内 catch_unwind，插件 panic 降级为 Err 不带崩宿主。

1. **同一 rustc**：插件和宿主用同一个 Rust 工具链编译。
2. **api crate 版本一致**：见上"握手"第 2 条；宿主业务变更不再牵连插件。
3. **lockfile 与根同步**：deps stamp 不一致即拒载（deploy 脚本已自动
   `cp Cargo.lock plugins/*/Cargo.lock`；手工构建插件时照做）。

## 现有插件

| 插件 | 目录 | 说明 |
|---|---|---|
| hello | `templates/plugin-hello/` | 最小模板 |
| competitions 等 13 个 | `plugins/*/` | 业务附属全部插件化（详见 src/plugins.rs 与部署文档） |

## 插件形态

```rust
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("<name>", || DEF)
}

static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "<name>",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("<name>"),   // 或 None
    schedule: |config| { /* JobSchedule 清单 */ },
    tracker: |config, store, name| { /* 命中才构造 */ },
    qq_handlers: |config, backend| { /* QQ 命令 */ },
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};
```

- DEF 与内建模块完全同构：配置读取（`config.sources.get::<T>(name)`）、
  vault、通知路由、`Arc<dyn TaskStore>` 全可用，签名一致。
- 插件**不用** linkme 自注册（dylib 独立 section，宿主 slice 看不到），
  必须走入口函数显式返回 DEF。
- `ModuleDef` 是 Copy，宿主 `Box::leak` 成 `'static`，插件 .so 永不卸载。

## 部署拓扑

插件目录默认 `/etc/ru41/plugins`（`[plugins] dir` 可改、`enabled = false` 可关）。
每个需要插件的容器都要把目录挂进去，例如 compose：

```yaml
volumes:
  - /home/windy2/ru41-plugins:/etc/ru41/plugins:ro
```

更新插件 = 替换 .so + 重启对应容器。宿主二进制不变。

## 与内建模块的分工

| | 内建模块（src/modules/） | 动态插件 |
|---|---|---|
| 注册 | linkme 自注册 | 入口函数返回 DEF |
| 编译 | 随主二进制 | 独立 .so |
| 上线 | 重新部署镜像 | 换 .so 重启容器 |
| 崩溃隔离 | 无（同进程） | 无（同进程） |
| 适合 | 核心团队维护的常驻能力 | 并行/第三方/试验性附属 |

ABI 破坏性变更（ru41-plugin-api 任何公开项、tokio/serde 大版本）时
bump `crates/ru41-plugin-api/Cargo.toml` 的 version，全部插件需重编；
`PluginEntry` 自身布局/握手协议变更时 `entry.rs` 的 `PLUGIN_API_VERSION` +1。

> **合并后账目（2026-10-28，exp/plugin-api 已并入 main）**：
> bfc2a69 把整体插件化合入 main，`ModuleDef` 正典移到 `crates/ru41-plugin-api`
> （5 字段含 `image_consumers`，`PLUGIN_API_VERSION` = 2）。注意两点：
> - 线上在跑的 13 个 `.so` 是 v1 握手——合并后的宿主首次部署前必须
>   `./deploy/plugins-build-all.sh PUSH=1` 全量重编插件，否则握手失败、
>   模块**静默消失**（启动日志只是"跳过"，不是报错）。
> - 模块级接线（DEF 字面量里的闭包）是合并/重构最易被扫成 noop 的位置，
>   chaoxing 照片签到曾因此断线——`tests/chaoxing_sources.rs` 的
>   `def_image_consumers_wires_qr_signer_when_ready` 已钉住，同类接线照此加测试。
