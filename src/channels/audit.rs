//! P2 出站审计（privacy-llm-boundary.md）：所有经 NotifyChannel 出站的文本
//! 先过红区扫描；命中即把敏感片段替换为占位符再发送（拦截=脱敏而非丢消息），
//! 并输出带当日去重的告警日志。这层防的是 P0/P1 漏网的，不是替代。

use crate::ports::channel::{MediaRef, NotifyChannel};
use crate::privacy::scan;
use anyhow::Result;
use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

/// 当日告警去重的进程级状态：(YYYY-MM-DD, 已告警的规则签名集合)。
static ALERT_STATE: Mutex<Option<(String, HashSet<String>)>> = Mutex::new(None);

pub struct AuditedNotifyChannel {
    inner: Arc<dyn NotifyChannel>,
}

impl AuditedNotifyChannel {
    pub fn wrap(inner: Arc<dyn NotifyChannel>) -> Arc<dyn NotifyChannel> {
        Arc::new(Self { inner })
    }
}

/// 记录告警：同一规则签名当日只报一次。（vault 提交审计等出站外的红线复用）
pub(crate) fn alert_once(rules_signature: &str, detail: &str) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut state = ALERT_STATE.lock().unwrap();
    let (date, alerted) = state.get_or_insert_with(|| (today.clone(), HashSet::new()));
    if *date != today {
        *date = today.clone();
        alerted.clear();
    }
    if !alerted.insert(rules_signature.to_owned()) {
        return;
    }
    eprintln!("[audit] 出站命中红区（当日首次告警）：{detail}");
}

/// 审计一段出站文本：返回脱敏后的文本；有命中时顺带告警。
pub fn audit_outbound(text: &str) -> String {
    let hits = scan::scan_text(text);
    if hits.is_empty() {
        return text.to_owned();
    }
    let mut rules: Vec<&str> = hits.iter().map(|hit| hit.rule).collect();
    rules.sort_unstable();
    rules.dedup();
    let signature = rules.join("+");
    alert_once(
        &signature,
        &format!("rules=[{signature}] lines={}", hits.len()),
    );
    scan::redact_secrets(text)
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
        let audited = audit_outbound(text);
        Box::pin(async move { self.inner.send(target, msg_id, &audited).await })
    }

    fn send_media<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        caption: &'a str,
        media: &'a MediaRef,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        let audited = audit_outbound(caption);
        Box::pin(async move { self.inner.send_media(target, msg_id, &audited, media).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_passes_untouched() {
        let text = "今日水电 12.3 元，记得查看";
        assert_eq!(audit_outbound(text), text);
    }

    #[test]
    fn secret_in_caption_is_redacted_and_deduped() {
        let first = audit_outbound("部署完成 password=Sup3rSecret99 请查收");
        assert!(!first.contains("Sup3rSecret99"));
        assert!(first.contains("password=<secret_kv>"));
        // 同签名第二次：仍然脱敏，但不再刷告警日志（去重逻辑在 alert_once 内）。
        let second = audit_outbound("再发一次 password=AnotherSecret88");
        assert!(!second.contains("AnotherSecret88"));
    }
}
