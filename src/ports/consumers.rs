use crate::domain::consumers::{ConsumerSpec, ConsumerState, DeliveryFailure, EventDelivery};
use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

/// 消费进度与认领必须持久化；确认、失败和续租必须校验代次、事件与未过期租约。
pub trait ConsumerStore: super::events::EventStore {
    /// 幂等注册；已有消费者的起点/重试预算不允许被静默覆盖。
    fn register_consumer(&self, spec: &ConsumerSpec) -> Result<ConsumerState>;
    fn consumer(&self, name: &str) -> Result<Option<ConsumerState>>;
    /// 每个消费者一次只认领一条；失败或崩溃后先重投当前事件。
    fn claim_event(&self, name: &str, now: i64, lease_secs: i64) -> Result<Option<EventDelivery>>;
    fn renew_event(&self, delivery: &EventDelivery, now: i64, lease_secs: i64) -> Result<bool>;
    fn complete_event(&self, delivery: &EventDelivery, now: i64) -> Result<bool>;
    fn fail_event(
        &self,
        delivery: &EventDelivery,
        now: i64,
        retry_at: i64,
        failure: DeliveryFailure,
    ) -> Result<bool>;
    /// 只恢复重试耗尽的消费者，保留当前事件，重新授予同样的尝试预算。
    fn retry_consumer(&self, name: &str, now: i64) -> Result<bool>;
}

impl<T: ConsumerStore + ?Sized> ConsumerStore for Arc<T> {
    fn register_consumer(&self, spec: &ConsumerSpec) -> Result<ConsumerState> {
        (**self).register_consumer(spec)
    }
    fn consumer(&self, name: &str) -> Result<Option<ConsumerState>> {
        (**self).consumer(name)
    }
    fn claim_event(&self, name: &str, now: i64, lease_secs: i64) -> Result<Option<EventDelivery>> {
        (**self).claim_event(name, now, lease_secs)
    }
    fn renew_event(&self, d: &EventDelivery, now: i64, secs: i64) -> Result<bool> {
        (**self).renew_event(d, now, secs)
    }
    fn complete_event(&self, d: &EventDelivery, now: i64) -> Result<bool> {
        (**self).complete_event(d, now)
    }
    fn fail_event(
        &self,
        d: &EventDelivery,
        now: i64,
        retry_at: i64,
        failure: DeliveryFailure,
    ) -> Result<bool> {
        (**self).fail_event(d, now, retry_at, failure)
    }
    fn retry_consumer(&self, name: &str, now: i64) -> Result<bool> {
        (**self).retry_consumer(name, now)
    }
}

/// 处理器成功返回后才确认进度；忽略不关心的事件也应成功返回。
/// 外部副作用可能被重试，处理器应使用 delivery.idempotency_key() 去重。
/// 丢弃 future 应停止本地工作；远端已接收的请求不保证撤回。
pub trait EventHandler: Send + Sync {
    fn handle<'a>(
        &'a self,
        delivery: &'a EventDelivery,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}
