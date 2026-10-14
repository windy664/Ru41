use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};
/// 一个通知渠道（如 QQ）；只负责把终态结果送达 target。
pub trait NotifyChannel: Send + Sync {
    fn channel_id(&self) -> &str;
    fn send<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

impl<T: NotifyChannel + ?Sized> NotifyChannel for Arc<T> {
    fn channel_id(&self) -> &str {
        (**self).channel_id()
    }
    fn send<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        (**self).send(target, msg_id, text)
    }
}
