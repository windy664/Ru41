use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

pub use ru41_plugin_api::ports::media::{MediaKind, MediaRef};

use crate::domain::NotifyRoute;

/// 主动推送的投递口（多渠道，按渠道分别记账）。
///
/// 提醒类消息（到点提醒、行程提醒、错过的课程提醒）不经过 dispatcher，
/// 但同样要双投（主渠道 + `[notify].fanout` 里的并行渠道）。
/// 记账与 dispatcher 同一套：某个渠道失败重试时，已送达的渠道不重发。
pub trait ProactiveChannel: Send + Sync {
    /// 投递一条主动消息。
    ///
    /// `delivery_id` 是这条消息的**稳定标识**（同一轮重试用同一个）——
    /// 幂等与「只补没送达的渠道」都靠它。返回 true = 所有目标渠道都已送达
    /// （调用方据此记录「已提醒」）。
    fn deliver<'a>(
        &'a self,
        route: &'a NotifyRoute,
        delivery_id: &'a str,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;
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

    /// 发送带快捷按钮的消息；渠道不支持的默认降级为纯文本发送
    /// （正文应保留操作指路，降级后用户仍知道能做什么）。
    fn send_with_markup<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
        _markup: &'a ru41_plugin_api::ports::commands::ReplyMarkup,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { self.send(target, msg_id, text).await })
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
    fn send_with_markup<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
        markup: &'a ru41_plugin_api::ports::commands::ReplyMarkup,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        (**self).send_with_markup(target, msg_id, text, markup)
    }
}
