//! 渠道无关的「可点指令」占位符契约。
//!
//! 规范层（插件）只产出中性占位符 `{{cmd:符号|指令}}`，不掺任何平台专有标签；
//! 网关在渲染时按能力决定怎么落地：
//!
//! - **QQ**：展开成行内 `<qqbot-cmd-input>`（点一下把指令填进输入框，两跳），
//!   这是 QQ 侧**唯一一处** cmd-input 适配（`channels::qq::render`）。
//! - **其它渠道**：降级成纯文本符号（`☐`），不漏 token；后续要做一跳交互
//!   再用按钮机制补。
//!
//! 之所以要有这层：待办复选框此前直接内嵌了 `<qqbot-cmd-input>`，在 Telegram
//! 上渲染成一坨标签文本；把平台细节收回网关后，插件只剩中性内容。

/// 占位符前缀。
const OPEN: &str = "{{cmd:";
/// 占位符后缀。
const CLOSE: &str = "}}";

/// 生成一个可点指令占位符：`{{cmd:符号|指令}}`。
pub fn cmd_token(symbol: &str, command: &str) -> String {
    format!("{OPEN}{symbol}|{command}{CLOSE}")
}

/// 文本里是否含可点指令占位符。
pub fn has_cmd_token(text: &str) -> bool {
    text.contains(OPEN)
}

/// 抽出文本里的可点指令：返回 `(占位符原文, 符号, 指令)`。
pub fn cmd_tokens(text: &str) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find(CLOSE) else { break };
        let inner = &after[..end];
        let (symbol, command) = match inner.split_once('|') {
            Some((symbol, command)) => (symbol, command),
            None => (inner, ""),
        };
        found.push((
            rest[start..start + OPEN.len() + end + CLOSE.len()].to_owned(),
            symbol.to_owned(),
            command.to_owned(),
        ));
        rest = &after[end + CLOSE.len()..];
    }
    found
}

/// 通用降级：把占位符换成纯符号（指令丢弃）。
pub fn strip_cmd_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find(CLOSE) else { break };
        let inner = &after[..end];
        let symbol = inner.split('|').next().unwrap_or("");
        out.push_str(symbol);
        rest = &after[end + CLOSE.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trip() {
        let token = cmd_token("☐", "/完成 abc123");
        assert_eq!(token, "{{cmd:☐|/完成 abc123}}");
        let found = cmd_tokens(&format!("- {token} 标题"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, "☐");
        assert_eq!(found[0].2, "/完成 abc123");
    }

    #[test]
    fn strip_keeps_symbol_only() {
        let text = format!(
            "- {} 标题 - {}",
            cmd_token("☐", "/完成 a"),
            cmd_token("☑", "/恢复 b")
        );
        assert_eq!(strip_cmd_tokens(&text), "- ☐ 标题 - ☑");
    }

    #[test]
    fn multiple_tokens_are_all_found() {
        let text = format!("{} {}", cmd_token("☐", "/a"), cmd_token("☑", "/b"));
        assert_eq!(cmd_tokens(&text).len(), 2);
        assert!(has_cmd_token(&text));
    }
}
