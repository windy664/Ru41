use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

pub use ru41_plugin_api::ports::media::{MediaKind, MediaRef};

/// 一个通知渠道（如 QQ）；只负责把终态结果送达 target。
pub trait NotifyChannel: Send + Sync {
    fn channel_id(&self) -> &str;
    fn send<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

    /// 发送富媒体；渠道不支持的默认降级为「文本 + 链接/路径」。
    fn send_media<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        caption: &'a str,
        media: &'a MediaRef,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let text = if caption.is_empty() {
                media.source.clone()
            } else {
                format!("{caption}\n{}", media.source)
            };
            self.send(target, msg_id, &text).await
        })
    }
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
    fn send_media<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        caption: &'a str,
        media: &'a MediaRef,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        (**self).send_media(target, msg_id, caption, media)
    }
}
