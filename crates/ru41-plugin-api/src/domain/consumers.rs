use super::events::DomainEvent;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// 消费者名是持久身份；更换处理语义应使用新名字，避免复用旧进度。
#[derive(Clone, Debug)]
pub struct ConsumerSpec {
    pub name: String,
    pub start_after: i64,
    pub max_attempts: i64,
}
impl ConsumerSpec {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.is_empty()
                && self.name.len() <= 128
                && self
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c)),
            "invalid consumer name"
        );
        ensure!(
            self.start_after >= 0 && (1..=100).contains(&self.max_attempts),
            "invalid consumer configuration"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConsumerState {
    pub name: String,
    pub start_after: i64,
    pub cursor: i64,
    pub max_attempts: i64,
    pub attempts: i64,
    pub generation: i64,
    pub active_event_id: Option<i64>,
    pub lease_until: Option<i64>,
    pub available_at: i64,
    pub blocked: bool,
    /// 固定错误类别，不持久化处理器错误正文。
    pub last_failure: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EventDelivery {
    pub consumer: String,
    pub event: DomainEvent,
    pub generation: i64,
    pub attempt: i64,
}
impl EventDelivery {
    /// 同一消费者对同一事件重试时不变；不同租约代次不能作为幂等键。
    pub fn idempotency_key(&self) -> String {
        format!("consumer:{}:event:{}", self.consumer, self.event.id)
    }
}

#[derive(Clone, Copy)]
pub enum DeliveryFailure {
    Handler,
    Timeout,
    Interrupted,
}
impl DeliveryFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Handler => "handler_failed",
            Self::Timeout => "timeout",
            Self::Interrupted => "interrupted",
        }
    }
}

/// 供存储适配器（宿主侧 sqlite/postgres）共用的租约截止计算。
pub fn lease_deadline(now: i64, seconds: i64) -> Result<i64> {
    ensure!(
        now >= 0 && (1..=86400).contains(&seconds),
        "invalid consumer lease"
    );
    now.checked_add(seconds)
        .ok_or_else(|| anyhow::anyhow!("consumer lease overflow"))
}
