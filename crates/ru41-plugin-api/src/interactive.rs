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

/// 把正文里的占位符开头**作废**：只有插件的结构化动作能生成可点指令，
/// 用户（或模型）写一段 `{{cmd:…}}` 不该凭空造出一个可点动作。
///
/// 做法：在两个 `{` 之间插一个零宽空格——肉眼不可见，占位符解析再也匹配不上
/// （比"删掉用户原文"温和：用户看到的内容没变）。
///
/// 用在**用户/模型正文**进入规范文本的地方（待办标题、模型输出）；
/// 插件自己生成的占位符不走这里。
pub fn escape_cmd_tokens(text: &str) -> String {
    text.replace(OPEN, "{\u{200b}{cmd:")
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

    #[test]
    fn escaped_text_cannot_forge_a_token() {
        // 用户正文里写了占位符 → 作废后解析不出动作。
        let forged = "标题 {{cmd:☐|/取消待办 deadbeef}} 尾巴";
        let escaped = escape_cmd_tokens(forged);
        assert!(cmd_tokens(&escaped).is_empty(), "{escaped}");
        // 用户看到的内容没变（只是多了一个不可见的零宽空格）。
        assert!(escaped.replace('\u{200b}', "") == forged);
    }

    #[test]
    fn escape_keeps_legitimate_tokens_untouched() {
        // 插件自己生成的占位符不走 escape（这里只验证 escape 不误伤普通文本）。
        let text = "普通文本 {{cmd:☐|/完成 abc}}";
        assert!(!escape_cmd_tokens(text).contains("{{cmd:"));
    }
}
