//! P2 出站审计核心（privacy-llm-boundary.md）：红区扫描 + 当日去重告警，
//! 供通知渠道包装器（channels/audit.rs）与 vault 提交审计（adapters/vault.rs）
//! 共用。审计是隐私域能力，住在这里；渠道层只做包装，反向依赖即层级倒挂。
//! 这层防的是 P0/P1 漏网的，不是替代。

use crate::privacy::scan::{self, SecretHit};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

/// 当日告警去重的进程级状态：(YYYY-MM-DD, 已告警的规则签名集合)。
static ALERT_STATE: Mutex<Option<(String, HashSet<String>)>> = Mutex::new(None);

/// 告警外送钩子：装配层把默认通知渠道接进来，命中即推给用户；
/// 未接线（如单测、纯 CLI）时只落 stderr 日志。
type AlertSink = Arc<dyn Fn(String) + Send + Sync>;
static ALERT_SINK: Mutex<Option<AlertSink>> = Mutex::new(None);

/// 注册告警外送钩子；只接受第一次装配（多进程各自接线，进程内多次调用以后到者为准会
/// 打乱 tracker 场景，先到先得最简单）。
pub fn set_alert_sink(sink: AlertSink) {
    let mut slot = ALERT_SINK.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        *slot = Some(sink);
    }
}

/// 记录告警：同一规则签名当日只报一次（stderr 日志 + 可选外送）。
pub fn alert_once(rules_signature: &str, detail: &str) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut state = ALERT_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let (date, alerted) = state.get_or_insert_with(|| (today.clone(), HashSet::new()));
    if *date != today {
        *date = today.clone();
        alerted.clear();
    }
    if !alerted.insert(rules_signature.to_owned()) {
        return;
    }
    let message = format!("[audit] 出站命中红区（当日首次告警）：{detail}");
    eprintln!("{message}");
    let sink = ALERT_SINK.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(sink) = sink {
        sink(message);
    }
}

/// 规则签名：去重排序后用 + 连接，作为当日告警去重键。
pub fn signature_of(hits: &[SecretHit]) -> String {
    let mut rules: Vec<&str> = hits.iter().map(|hit| hit.rule).collect();
    rules.sort_unstable();
    rules.dedup();
    rules.join("+")
}

/// 审计一段出站文本：返回脱敏后的文本；有命中时顺带告警。
pub fn audit_outbound(text: &str) -> String {
    let hits = scan::scan_text(text);
    if hits.is_empty() {
        return text.to_owned();
    }
    let signature = signature_of(&hits);
    alert_once(
        &signature,
        &format!("rules=[{signature}] lines={}", hits.len()),
    );
    scan::redact_secrets(text)
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

    #[test]
    fn signature_dedupes_and_sorts_rules() {
        let hits = scan::scan_text("password=AbcDef123456 bearer Bearer abc.def.ghi");
        assert!(!hits.is_empty());
        let signature = signature_of(&hits);
        let rules: Vec<&str> = signature.split('+').collect();
        let mut sorted = rules.clone();
        sorted.sort_unstable();
        assert_eq!(rules, sorted);
        assert_eq!(rules.len(), rules.iter().collect::<HashSet<_>>().len());
    }
}
