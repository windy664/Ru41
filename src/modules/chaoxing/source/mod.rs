//! 学习通协议层。按职责拆分：`model` 领域类型、`client` HTTP 客户端、
//! `parse` 页面解析；对外路径保持 `chaoxing::source::*` 不变。

mod client;
mod model;
mod parse;

pub use client::ChaoxingClient;
pub use model::{
    ChaoxingActivity, ChaoxingCourse, ChaoxingSignKind, ChaoxingWork, CourseEncs, SignMethod,
    fallback_photo_bytes,
};
