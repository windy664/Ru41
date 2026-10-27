# 动态插件开发指南（cdylib 热插拔）

ru41 支持 Pumpkin/bevy 式动态插件：独立 crate 编译成 `.so`，扔进插件目录，
宿主启动时加载注册——**不碰主仓库、不重编主二进制**即可上线新附属。

适合：模块多团队并行开发、第三方贡献、快速试错。
不适合替代容器隔离：插件与宿主同进程，panic 可带崩宿主。

## 快速开始

```bash
cp -r templates/plugin-hello /your/workspace/ru41-plugin-foo
cd ru41-plugin-foo
# 编辑 Cargo.toml 的 name 与 src/lib.rs 的模块名
cargo build --release
cp target/release/libru41_plugin_foo.so <服务器>:/etc/ru41/plugins/
# 重启需要它的容器（poll/run 类重启模块容器；QQ 命令类重启 qq 容器）
```

验证：`ru41 schedule list` 出现模块名；`ru41 <name> poll` 输出 JSON。

## 硬约束（违反 = ABI 未定义行为，不是报错）

1. **同一 rustc**：插件和宿主用同一个 Rust 工具链编译。
2. **同一 ru41 版本**：Cargo.toml 里 `ru41 = { git = "...", rev = "<宿主部署的 commit>" }`，
   宿主升级后插件必须改 rev 重编。入口握手会校验 `CARGO_PKG_VERSION`，
   不一致拒绝加载（日志 `[plugins] 跳过 ...ru41 版本不匹配`）。
3. **同一依赖锁**：tokio/serde 等共享依赖版本必须与宿主一致——
   把主仓库的 `Cargo.lock` 复制进插件仓库最稳妥。

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

ABI 破坏性变更（ModuleDef 字段、trait 签名、tokio/serde 大版本）时
`src/plugins.rs` 的 `PLUGIN_API_VERSION` +1，全部插件需重编。
