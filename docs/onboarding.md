# 开发者上手（接手指南）

目标：新开发者（或新 agent 底座）从 clone 到第一次把插件改动部署上线，半天内走通。
本文是**地图**，只串联路径不复制内容；细节一律跳转对应深文档。

## 0. 五分钟理解这个项目

Ru41 是 Rust 个人自动化宿主：核心任务调度器（SQLite/Postgres + Worker + 渠道网关）
驱动 agent 底座（Codex / Pi）执行任务，**23 个附属功能全部是 cdylib 插件（.so）**，
启动时 dlopen 挂载。没有内建业务模块——`src/modules/` 只剩注册表空壳。

- 产品视角：[产品设计](product-design.md) + 根 [README](../README.md)
- 能力边界（什么没做完）：[核心宿主状态](core-status.md)
- 插件↔宿主边界与 ABI 握手：[动态插件开发指南](plugin-development.md)

## 1. 环境准备

- rustc 1.99（与 CI 一致）、clang + lld（插件共享链接器）、docker（净室编译/部署）。
- **先读 [工程约定](conventions.md)**——它是活着的构建契约：目录边界、共享
  target、bridge 纪律、部署分层、红线，全部在里面。

## 2. 本地跑通（不改任何代码）

```bash
cargo build                       # 只覆盖核心 src/ + tests/（插件不在 workspace）
cargo test --locked               # 测试用模拟 Agent/假进程，不调付费模型
cargo run -- schedule list        # 看全部已登记定时任务与启停原因
cargo run -- --help               # CLI 总览；ru41 init / doctor 可离线自检
```

跑真实数据源需要 `ru41.toml`（抄 `ru41.example.toml` 填凭证，永不入 git）。
插件产物编译见下节。

## 3. 第一次插件改动（最小闭环）

以改一个既有插件（如 `plugins/jwxt`）为例：

```bash
cd plugins/jwxt
cargo build --release --features plugin-entry   # 必须带 feature，否则无入口符号
cargo test                                      # 插件测试独立跑，-p 找不到包
```

本地热验证（不碰生产）：

```bash
# 用与镜像同 glibc 的净室容器编（本机 glibc 新于容器会 dlopen 失败）
docker run --rm -v "$PWD/..":/build -w /build/jwxt \
  -e CARGO_TARGET_DIR=/build/jwxt/target-docker \
  rust:1.98-trixie cargo build --release --features plugin-entry
# 手头有 ru41.toml + state.db 时可以直接本机跑：
# ./target/release/ru41 --config ru41.toml schedule run <job>
```

部署到生产（热更，两分钟）：

```bash
scp target-docker/release/libru41_plugin_jwxt.so \
    server:部署目录/plugins/            # 容器 bind mount，替换 .so
ssh server docker restart ru41-<相关容器>
docker logs ru41-<容器> --since 5m     # 验证轮询报告，无 panic
```

走完整镜像链（改动涉及核心 `src/`、`crates/ru41-plugin-api`、技能包时必须）：

1. `cargo fmt --all` → `git push origin main main:exp/plugin-api`
2. CI 构建 `ghcr.io/windy664/ru41:<SHA>`（约 10 分钟，`gh run view <id>`）
3. 本机 `docker pull` → `docker save | ssh server docker load`
4. 服务器 `docker tag … ru41:local` → `docker create`+`docker cp` 抽插件 → `bash ~/ru41-roll.sh`
5. 验证 `docker ps | grep -c Up`（应 ~32）、逐容器日志

## 4. 新写一个插件

模板 `templates/plugin-hello`，完整步骤（ModuleDef 六钩子、握手三硬门、
bridge 纪律、技能包）见[动态插件开发指南](plugin-development.md)。
三个最容易翻车的点，先记住：

1. **异步入口必须过 `ru41_plugin_api::bridge`**——插件 .so 静态链接自己的
   tokio，宿主 reactor 对它不可见；不过桥在网络调用时 panic 并 abort 宿主
   进程。核心 tracker 类型被插件实例化时同样适用（2026-11-04 课前提醒全丢事故）。
2. **编 .so 必须 `--features plugin-entry`**，否则缺 `ru41_plugin_entry`
   导出符号，宿主「跳过」加载且不报错。
3. **插件不在 workspace**：`cargo test -p <名>` 无效；进插件目录单独跑，
   共享 target 在 `plugins/target-plugins/`（**别在里面 cargo clean**）。

## 5. 生产拓扑速查（单机 docker，31+ 容器）

- 一个宿主镜像 `ru41:local`，多容器 = 同镜像不同入口：
  `ru41-<插件>`（各 tracker 轮询）、`ru41-qq`（网关）、`ru41-mimo-worker`
  （agent 执行）、`ru41-browser`（无头浏览器）、`ru41-todo-remind` /
  `ru41-class-remind`（到点提醒）等；外部还有 mihomo（代理）与
  paraformer-asr（课堂录音转写，whisper.cpp 兼容接口）。
- 部署目录 bind mount：`~/ru41-mimo-smoke → /etc/ru41`（配置 + 插件 .so +
  技能包），`~/ru41-mimo-smoke/data → /data`（state.db、登录态、模型缓存）。
- 日志分散在各容器：`docker logs ru41-<名> --since 5m`；没有总日志，排障
  先 `docker ps` 找相关角色容器再看。
- 转写/ASR 等 AI 推理**不跑在宿主容器里**（无 torch），重依赖独立容器。

## 6. 排障速查

| 症状 | 先查 |
|---|---|
| 容器反复重启、panic "no reactor running" | 异步入口没过 bridge（见 §4.1） |
| 插件静默消失，日志只有「跳过 .so」 | 缺 plugin-entry feature / glibc 不匹配 / 握手三硬门任一失败 |
| CI 只红 fmt/clippy | 本地 `cargo fmt --all` + `cargo clippy --locked --all-targets -- -D warnings`，本地绿=CI 过 |
| 某源轮询报「openid/登录态失效」 | 见[凭证总览](credentials.md)的会话文件清单与更换操作 |
| 改了插件但行为没变 | .so 没刷新；技能包未同步（`deploy/hot-patch.sh` 会一并同步 skills，完整链由 `roll.sh` 同步） |
| schedule run 说 unknown job | 该插件没加载成功，回上一行 |

## 7. 继续深入的文档地图

- 命令/配置/HTTP API 全集：[usage.md](usage.md)
- 凭证注入路径与换 key 操作：[credentials.md](credentials.md)
- 各数据源逆向笔记与协议红线：`docs/sources-*.md`、`docs/*-recon.md`
- 渠道协议细节：[channels-qq.md](channels-qq.md) / [channels-feishu.md](channels-feishu.md)
- Agent 底座接入：[agent-backends.md](agent-backends.md)
- Docker/备份/重依赖扩展：[deploy/README.md](../deploy/README.md)
- 未完成项与技术债（接手后从哪继续）：[core-status.md](core-status.md)、[roadmap.md](roadmap.md)
