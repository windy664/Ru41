//! 通义听悟（Aliyun Tingwu）客户端：**音视频 → 逐字稿**。
//!
//! ## 为什么在宿主 crate 而不是插件里
//!
//! 2026-11-26 实测踩到：原先客户端在 `plugins/tingwu` 里，`lecture` 插件
//! 想「先走听悟、兜底本地」就得跨插件调用。两个插件是各自静态链接的
//! cdylib（连 `ru41-plugin-api` 都是各一份副本）—— **插件之间没有任何
//! 共享状态的通道**（全局注册表在两份副本里各存各的，查不到对方注册的）。
//! 所以客户端上移到宿主 crate：`lecture` 与 `tingwu` 插件都用这一份。
//!
//! ## 认证
//!
//! 走**网页版内部 API**（不是官方 OpenAPI —— 那套要 `Authorization: Bearer
//! <transToken>`，而 transToken 的来源至今没在 umi.js 里找到）。网页版用
//! **cookie** 认证，能直接用用户自己的账号额度。cookie 从 ru41 的
//! **凭据托管库**取，真值不进仓库、不进模型上下文。
//!
//! ## 链路（参数逐字段对齐网页版，少一个字段就 DefaultError）
//!
//! 1. `POST /api/trans/request?generatePutLink`（**useSts=0**）→ OSS 预签名
//!    **putLink**（上传用）+ **getLink**（同步用）+ transId
//!    ⚠️ useSts=1 时服务端只给 sts 不给 putLink（网页版改用 STS 凭证直传 OSS，
//!    那条路要引 OSS SDK，太重）—— 所以这里必须 useSts=0。
//! 2. `PUT putLink`（原始字节，Content-Type 用文件 MIME）→ 文件进 OSS
//! 3. `POST /api/trans/request?syncPutLink`（**fileLink=getLink**！网页版
//!    传的就是 getLink）→ 服务端开始转写
//!    ⚠️ **没有** startTrans 这个 action —— syncPutLink 成功即自动开转（实测）
//! 4. `POST /api/trans/request?getTransStatus` 轮询 → status 0=成功 1=转写中 2=失败
//! 5. `POST /api/trans/getTransResult` → 逐字稿 JSON 字符串
//!
//! ## 逐字稿结构
//!
//! `{pg:[{pi,sc:[{bt,et,id,si,tc}],ui}]}` —— bt/et 毫秒、si 说话人、tc 文本。
//! **空稿**时服务端给的是 `{"sc":[]}`（没有 pg）—— 两种都要认。
//!
//! ## si 有两种语义（2026-11-26 实测）
//!
//! - `speaker`：多人对话，si 会**回退**（A,B,A）→ 按 si 分段才有说话人分离的价值。
//! - `turn-index`：单人连讲（课堂录音），si **单调递增**且从不回头
//!   （实测 75 分钟课堂课 8518 句被切成 si 1..716，平均每个 si 只 12 个字）——
//!   按 si 分会把逐字稿切成一堆碎片。命中时归一化成一个人，再按停顿分段。

pub mod client;
pub mod config;

pub use client::{
    Segment, Sentence, TingwuClient, Transcript, format_time, format_ts, group_by_speaker,
    is_turn_index, mime_for, normalize_turn_index_speakers, parse_sentences, render_markdown,
    render_srt, render_text,
};
pub use config::TingwuConfig;
