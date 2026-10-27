use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// 待办视图的窗口配置（TodoSection::options 的产出，配置边界附带的小值类型）。
/// 其余 todo 域类型（TodoItem/parse_due/due_label 等）不跨插件边界，留在宿主。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoOptions {
    /// 用户本地时区固定偏移秒数；部署在容器内时不依赖宿主机时区。
    pub offset_seconds: i32,
    /// “近期”窗口天数。
    pub recent_days: i64,
    /// 超过该窗口的已排期事项归入长期。
    pub long_term_days: i64,
}

impl Default for TodoOptions {
    fn default() -> Self {
        Self {
            offset_seconds: 8 * 3600,
            recent_days: 7,
            long_term_days: 14,
        }
    }
}

impl TodoOptions {
    pub fn validated(self) -> Result<Self> {
        ensure!(
            (-12 * 3600..=14 * 3600).contains(&self.offset_seconds),
            "todo timezone offset must be between -12h and +14h"
        );
        ensure!(
            (1..=366).contains(&self.recent_days),
            "todo recent window must be 1..366 days"
        );
        ensure!(
            self.long_term_days >= self.recent_days && self.long_term_days <= 3660,
            "todo long-term window must be >= recent window and <= 10 years"
        );
        Ok(self)
    }
}
