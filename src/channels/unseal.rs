//! 出站解封通道：把消息里的凭据标识在**发出前**换成真值。
//!
//! 为什么(2026-11-24 用户要求「网关发出时执行一轮匹配再替换成明文」):
//! LLM 上下文里只该有**标识**(`cred_xxx`)，真值等消息真正发出去时才落地 ——
//! 于是模型、模型供应商的日志、会话记录里都没有明文，用户收到的却是能直接用的明文。
//!
//! 这个装饰器包住通道适配器（QQ / Telegram / 飞书），所有出站文本都过一遍解封：
//! 入站回复、任务终态通知、主动推送、提醒 —— 全覆盖（它们最终都汇到
//! [`NotifyChannel::send`] / [`NotifyChannel::send_with_markup`]）。
//!
//! ⚠️ 真值只在**宿主进程内**从凭据库读到并直接交给渠道，不进日志、不回显给模型。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;

use crate::domain::credential;
use crate::ports::channel::NotifyChannel;
use crate::ports::storage::TaskStore;

/// 带解封的通道：转发前把凭据标识换成真值。
pub struct UnsealingChannel {
    inner: Arc<dyn NotifyChannel>,
    store: Arc<dyn TaskStore>,
}

impl UnsealingChannel {
    pub fn new(inner: Arc<dyn NotifyChannel>, store: Arc<dyn TaskStore>) -> Self {
        Self { inner, store }
    }

    fn unseal(&self, text: &str) -> String {
        credential::unseal_text(self.store.as_ref(), text)
    }
}

impl NotifyChannel for UnsealingChannel {
    fn channel_id(&self) -> &str {
        self.inner.channel_id()
    }

    fn send<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        let prepared = self.unseal(text);
        let inner = self.inner.clone();
        Box::pin(async move { inner.send(target, msg_id, &prepared).await })
    }

    /// 带快捷按钮的消息同样要解封 —— QQ 覆盖了这个方法（直接调 API，绕过 `send`），
    /// 装饰器不覆盖它的话，带键盘的消息就会漏掉替换。
    fn send_with_markup<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
        markup: &'a crate::ports::commands::ReplyMarkup,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        let prepared = self.unseal(text);
        let inner = self.inner.clone();
        Box::pin(async move {
            inner
                .send_with_markup(target, msg_id, &prepared, markup)
                .await
        })
    }

    /// 富媒体的 caption 也要解封；默认实现会转成 `send`，但这里显式覆盖更稳。
    fn send_media<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        caption: &'a str,
        media: &'a crate::ports::channel::MediaRef,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        let prepared = self.unseal(caption);
        let inner = self.inner.clone();
        Box::pin(async move { inner.send_media(target, msg_id, &prepared, media).await })
    }
}
