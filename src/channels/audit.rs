//! P2 出站审计的通知渠道包装器：所有经 NotifyChannel 出站的文本先过
//! `privacy::egress` 红区扫描，命中即脱敏后发送（拦截=脱敏而非丢消息）。
//! 审计逻辑本体在 privacy/egress.rs，这里只做 trait 适配。

use crate::ports::channel::{MediaRef, NotifyChannel};
use crate::privacy::egress;
use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

pub struct AuditedNotifyChannel {
    inner: Arc<dyn NotifyChannel>,
}

impl AuditedNotifyChannel {
    pub fn wrap(inner: Arc<dyn NotifyChannel>) -> Arc<dyn NotifyChannel> {
        Arc::new(Self { inner })
    }
}

impl NotifyChannel for AuditedNotifyChannel {
    fn channel_id(&self) -> &str {
        self.inner.channel_id()
    }

    fn send<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        let audited = egress::audit_outbound(text);
        Box::pin(async move { self.inner.send(target, msg_id, &audited).await })
    }

    fn send_media<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        caption: &'a str,
        media: &'a MediaRef,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        let audited = egress::audit_outbound(caption);
        Box::pin(async move { self.inner.send_media(target, msg_id, &audited, media).await })
    }
}
