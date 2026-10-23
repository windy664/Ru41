//! 学习通服务层。按作业域拆分：`sign` 签到监控、`work` 作业监控；
//! 对外路径保持 `chaoxing::service::*` 不变。

mod qr;
mod sign;
mod work;

pub use crate::modules::chaoxing::config::ChaoxingConfig;

pub use qr::ChaoxingQrSigner;
pub use sign::{ChaoxingPollReport, ChaoxingTracker, sign_type_name};
pub use work::{ChaoxingWorkPollReport, ChaoxingWorkTracker};
