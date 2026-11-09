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
| blog / competitions / compshare / ecard / homework / jobs / lecture / notices / pagewatch / papers / programs / projects / research / rss / zhihuishu | `plugins/*/` | 业务附属全部插件化（共 15 个，配置与运行细节见 `docs/sources-*.md`） |
| library | `plugins/library/` | 图书馆：CAS→超星统一认证→libsp，到期催还提醒（3/1/0 天分级）、`/借阅`、`/续借`（带按钮）、`/找书`（免登录公开馆藏检索，索书号+在架）；协议见 `docs/library-recon.md` |
| events | `plugins/events/` | 活动日程：茶思屋会议日历 + 活动行大湾区三城 → 科技向筛选 → vault `Events-活动/活动总览.md`；只写 Obsidian 不推送 |
| youtube | `plugins/youtube/` | YouTube 英语预习：频道轮询 + `/英语` QQ 命令 + `ru41 youtube prep` 专属 CLI（cli 钩子的参考实现） |
| teachermate | `plugins/teachermate/` | 微助教：openid 直连签到监控 + 作业提醒 + `ru41 teachermate work/bind/sign` 专属 CLI（cli 钩子多子命令参考实现）。**2026-11-08 生产已下线**（openid 寿命 ~2h，需人工微信复制链接续命，上线零数据；容器已停删，代码保留随时可复活） |
| chaoxing | `plugins/chaoxing/` | 学习通：签到监控（全类型 sign + 二维码照片消费者）+ 作业提醒 + `ru41 chaoxing work/sign` 专属 CLI（image_consumers 接线测试被 DEF 直调钉住） |
| jwxt | `plugins/jwxt/` | 教务课表：CAS 登录 + 变更检测 + 明日课表提醒 + `ru41 jwxt classes/login` 专属 CLI（fixtures 自包含） |
| planner | `plugins/planner/` | 待办与日程：todo-remind job + `#` 待办入口 + `/日程` QQ 命令（读 domain::timetable 课表契约，最小 JwxtSemesterRef 取学期起点） |
| obsidian | `plugins/obsidian/` | Obsidian 同步入口：vault 同步 job（业务服务在核心 app::obsidian，本插件只做 DEF 装配；`#临时想法` 快记归 planner 的待办入口） |

**至此 src/modules/ 清空**（2026-11-03）：所有附属插件化完成，内建注册表只剩空壳，
`/日程` 合并课表走 `ru41::domain::timetable` 数据契约，不依赖任何插件代码。

## 本地开发与测试

- 插件**不在根 workspace**：根目录的 `cargo test` / `cargo clippy` 跑不到插件，
  测试要 `cd plugins/<name> && cargo test`（clippy 同理）。
- 各插件的 `Cargo.lock` 由部署/镜像构建脚本从根 lock 同步覆盖；本地看到
  插件 lock 大段 diff 属正常（依赖树被根重解析），照根提交即可。
- 正常发版**不需要手动 build 插件**：镜像 `Dockerfile` 的 `plugins` stage
  会用与宿主同源同锁的源码树全量重编并打进 `/etc/ru41/plugins`，镜像即
  完整交付物。`deploy/plugins-build-all.sh` 只在 ABI bump 后的应急场景用
  （见下方合并后账目）。

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
    // 专属 CLI（ru41 <name> <子命令>，poll|run 之外）才实现；签名
    // (config, store, args)，注意返回的 future 必须 'static——先把配置克隆
    // 成 owned 再进 async move。参考实现：plugins/youtube（prep）。
    cli: ru41::modules::no_cli,
};
```

- DEF 与内建模块完全同构：配置读取（`config.sources.get::<T>(name)`）、
  vault、通知路由、`Arc<dyn TaskStore>` 全可用，签名一致。
- 插件**不用** linkme 自注册（dylib 独立 section，宿主 slice 看不到），
  必须走入口函数显式返回 DEF。
- `ModuleDef` 是 Copy，宿主 `Box::leak` 成 `'static`，插件 .so 永不卸载。

## QQ 命令（SimpleCommand）

`qq_handlers` 里注册的命令统一实现 `ports::commands::SimpleCommand`
（2026-11-08 起）：只填 `verbs()` / `help()` / `run(arg, ctx)`，verb 匹配、
`Pin<Box<dyn Future>>` 仪式、多命令词分发（`run_command`）、自然语言兜底
（`run_message`）由 plugin-api 的统一实现代劳，实现它即自动获得
`CommandHandler`，注册处不用改。`help_section()` 可声明帮助页分区
（记录与日程 / 课程与学习 / 信息与学习 / 校园生活），缺省进入“其他能力”。

```rust
impl SimpleCommand for FooQqCommand {
    fn verbs(&self) -> &'static [&'static str] { &["foo", "富"] }
    fn help(&self) -> &'static str { "`/foo`：……" }
    fn help_section(&self) -> &'static str { "其他能力" }
    async fn run(&self, arg: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        if arg.is_empty() { return Ok(None); }  // 不拦截，交回 Agent
        Ok(Some(Reply::text("……")))
    }
}
```

⚠️ 触网调用仍须显式过 `ru41::plugins::bridge`（cdylib 的 tokio reactor 与
宿主不通用，漏了第一个 reqwest await 就炸网关）——这一步样板代劳不了。
参考：plugins/pagewatch（最简）、plugins/library（bridge + 多命令词）。

## 定时任务（SimplePollJob）

`tracker` 返回的轮询任务统一实现 `ingress::schedule::SimplePollJob`
（2026-11-08 起）：只填 `interval()` 和 `poll()`，bridge 与报告序列化由
统一实现代劳，实现它即自动获得 `PollJob`——手抄 `poll_json` 漏 bridge
造成的崩溃循环（2026-11-04 class-remind）在类型上不可能再犯。

```rust
impl ru41::ingress::schedule::SimplePollJob for FooTracker {
    type Report = FooReport;              // serde::Serialize + Send + 'static
    fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }
    async fn poll(&self) -> anyhow::Result<Self::Report> {
        self.poll_once().await            // 业务方法保持原样，可直接触网
    }
}
```

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

## 附录：内建模块迁插件 checklist（2026-11-03 youtube 试点沉淀）

目标：把 `src/modules/<m>/` 迁成 `plugins/<m>/`，核心只减不增。

1. **机械搬移**：`./deploy/migrate-module-to-plugin.sh <m>`
   （git mv + 引路改写 + 入口函数 + Cargo.toml 模板；已钉住 youtube 试点
   发现的三个坑：lib.rs 自引用还原、子模块测试 `use super::*`、根级 `super::no_*`）。
2. **Cargo.toml 补依赖**：模板只含常见依赖；正式代码用到的
   （如 tempfile）必须进 `[dependencies]`，只测试用的进 `[dev-dependencies]`。
3. **核心侧移除**（全是机械删）：
   - `src/ingress/cli.rs`：删 `Command::<M>` 变体与 `<M>Command` 枚举
     （`ru41 <m> poll|run` 由 external_subcommand 兜底，专属子命令走 cli 钩子）。
   - `src/main.rs`：删 import 与对应 match 臂；专属命令逻辑搬进插件 cli 钩子。
   - `src/bootstrap/assembly.rs`：删为该模块写的转发函数与 import。
   - `src/modules/mod.rs`：删 `pub mod <m>;`（脚本已做）。
   - `tests/schedule_jobs.rs`：该模块断言移除或换成其他内建模块。
4. **集成测试搬家**：`git mv tests/<m>_sources.rs plugins/<m>/tests/`，
   `ru41::modules::<m>` → `ru41_plugin_<m>`（注意多行 use 块里的残留要拆成独立 use）。
5. **专属 CLI（如有）**：实现 `DEF.cli`（签名 `(config, store, args)`，
   future 必须 `'static`：先把 section/参数克隆成 owned 再 `async move`）。
   `main.rs` 的 Tracker 分发已泛化：`poll|run` 之外的参数一律进 cli 钩子。
6. **ABI 门**：改了 ru41-plugin-api 任何公开项 → bump 它的 `version`
   （旧 .so 会被握手正确拒载，部署时随镜像全量重编，无需手动）。
6.5 **异步边界（迁移模块最容易漏，2026-11-03 五连 crash 换来的教训）**：
   插件静态链接自己的 tokio，reactor 与宿主不通用——模块时代裸写的
   `Box::pin(async move { ... })` 一律改 `ru41::plugins::bridge(async move {...})`：
   - 每个 `impl PollJob` 的 `poll_json`（tracker 结构体补 `#[derive(Clone)]`，
     `let owned = self.clone();` 进桥）
   - `DEF.cli` 钩子的整个异步体
   - feature 门代码（local-embed 等）在插件副本里是占位实现：
     依赖它们的接线要降级容错（如 obsidian 知识库 Err → 无知识模式），
     插件 .so 一律不带 ONNX。
7. **验证顺序**：`cd plugins/<m> && cargo test` → 根 `cargo test`
   → `cd plugins/<m> && cargo build --release --features plugin-entry` 后 `nm -D` 看
   `ru41_plugin_entry` → 临时配置 `[plugins] dir = "plugins/target-plugins/release"` 跑
   `schedule list`（看到本插件 job + 旧 .so 被拒日志）与 `ru41 <m> <cmd>` 端到端。
   ⚠️ 编插件必须 cd 进插件目录（或显式 `CARGO_TARGET_DIR=plugins/target-plugins`）：
   `--manifest-path` 从外部调用时 cargo 发现不了 `plugins/.cargo/config.toml`，
   会把产物散进 `plugins/<m>/target/`（共享缓存失效 + 重蹈每插件 target 覆辙）。
8. **部署**：正常发版不用手动编 .so——镜像 CI 的 plugins stage 全量重编；
   热更才 scp 单个 .so + 重启对应容器。
