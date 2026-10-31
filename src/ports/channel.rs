use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

/// 富媒体类型（渠道无关）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Voice,
    File,
}

/// 富媒体来源：http(s) URL 渠道直接引用，其余按本地文件路径上传。
#[derive(Clone, Debug)]
pub struct MediaRef {
    pub kind: MediaKind,
    pub source: String,
    /// 文件类附件的展示名；缺省取路径/URL 末段。
    pub filename: Option<String>,
}

impl MediaRef {
    pub fn new(kind: MediaKind, source: impl Into<String>) -> Self {
        Self {
            kind,
            source: source.into(),
            filename: None,
        }
    }

    pub fn is_remote(&self) -> bool {
        self.source.starts_with("http://") || self.source.starts_with("https://")
    }

    pub fn display_name(&self) -> &str {
        self.filename.as_deref().unwrap_or_else(|| {
            self.source
                .rsplit(['/', '\\'])
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("附件")
        })
    }
}

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
