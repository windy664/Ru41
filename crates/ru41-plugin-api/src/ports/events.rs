use crate::domain::events::DomainEvent;
use anyhow::Result;
use std::sync::Arc;

/// 事件日志的只读端口；写入必须由状态更新在同一事务中完成。
/// 实现必须保证后来提交的事件不会出现在已读取的游标之前。
pub trait EventStore: Send + Sync {
    /// 返回 id > after 的事件，按 id 升序；limit 为 1..=1000。
    fn events_after(&self, after: i64, limit: u32) -> Result<Vec<DomainEvent>>;
    /// 返回当前已提交事件的最大游标；空日志为 0。
    fn event_cursor(&self) -> Result<i64>;
}
impl<T: EventStore + ?Sized> EventStore for Arc<T> {
    fn events_after(&self, after: i64, limit: u32) -> Result<Vec<DomainEvent>> {
        (**self).events_after(after, limit)
    }
    fn event_cursor(&self) -> Result<i64> {
        (**self).event_cursor()
    }
}
