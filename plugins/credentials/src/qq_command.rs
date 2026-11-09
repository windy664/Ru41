//! 凭据托管的自然语言入口：「帮我记住 ftp 的密码是 xxx」在网关层拦截入库，
//! 消息不交给 agent——pi 会话记录与任务库都不留明文。
//! 名单与删除交给 agent 经核心 CLI（ru41 secret list/delete）自然语言完成，
//! 这里不做任何斜杠命令。

use anyhow::{Result, ensure};
use ru41::ports::commands::{CommandContext, Reply, SimpleCommand};

/// QQ 凭据托管：无命令，只拦自然语言的「记住密码」。
pub struct CredentialQqCommand;

const KIND: &str = "credential";

impl SimpleCommand for CredentialQqCommand {
    fn verbs(&self) -> &'static [&'static str] {
        &[]
    }

    fn help(&self) -> &'static str {
        "说「帮我记住 xx 的密码是 xxx」：凭据入库且不进 AI 会话；用时 agent 自取（真值不进模型上下文）"
    }

    async fn run(&self, _arg: &str, _ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        Ok(None) // 无命令词，永远不会命中；自然语言全走 run_message。
    }

    async fn run_message(&self, content: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        let Some((label, value)) = parse_store_intent(content) else {
            return Ok(None);
        };
        if let Some(reply) = ctx.require_private("存凭据", "凭据属于个人信息")? {
            return Ok(Some(Reply::text(reply)));
        }
        let now = ru41::util::now();
        // 凭据只有唯一标识；用户说的词只是标签（给人认，不参与寻址）。
        let id = ru41::domain::credential::new_id();
        let revision = ctx.store.document(KIND, &id)?.map(|doc| doc.revision);
        let written = ctx.store.put_document(
            KIND,
            &id,
            revision,
            &serde_json::json!({
                "value": value,
                "label": label,
                "updated_at": now,
            }),
        )?;
        ensure!(written, "凭据写入冲突，请重试");
        Ok(Some(Reply::text(format!(
            "## ✅ 已存凭据 `{id}`\n\n标签：{label}\n这条消息没进 AI 会话，建议长按撤回。agent 用时经 `secret exec` 注入，真值不进模型上下文。"
        ))))
    }
}

/// 「帮我记住 ftp 的密码是 xxx」→ Some(("ftp", "xxx"))。
/// 必须有存储意图词 + 凭据关键词 + ASCII 值（防「我密码是多少」这类误命中）。
/// 名称缺省时落「默认」。
fn parse_store_intent(content: &str) -> Option<(String, String)> {
    let text = content.trim();
    const INTENTS: &[&str] = &[
        "帮我记住",
        "帮我记一下",
        "帮我存",
        "帮我记",
        "记住",
        "记一下",
        "存一下",
        "存起来",
        "存个",
        "托管",
        "别忘",
    ];
    let mut body: Option<String> = None;
    for intent in INTENTS {
        if let Some(pos) = text.find(intent) {
            body = Some(format!("{}{}", &text[..pos], &text[pos + intent.len()..]));
            break;
        }
    }
    let body = body?;

    // 凭据关键词：取最靠前的命中，前面是名称、后面是值。
    const KEYWORDS: &[&str] = &[
        "密码", "口令", "凭据", "密钥", "令牌", "token", "Token", "TOKEN",
    ];
    let (kw_pos, kw) = KEYWORDS
        .iter()
        .filter_map(|kw| body.find(kw).map(|pos| (pos, *kw)))
        .min_by_key(|(pos, _)| *pos)?;

    let label = body[..kw_pos]
        .trim()
        .trim_start_matches(['把', '将'])
        .trim_end_matches(['的', '我'])
        .trim_start_matches("我的")
        .trim()
        .replace(' ', "");
    let label = if label.is_empty() {
        "默认".to_owned()
    } else {
        label
    };
    ensure_name(&label).ok()?;

    let value = body[kw_pos + kw.len()..]
        .trim_start_matches(['是', ':', '：', '为', ' '])
        .split_whitespace()
        .next()?
        .to_owned();
    // 值必须是 ASCII 可见字符 4-64 位：中文问句（「是多少」「忘了」）不会命中。
    if !(4..=64).contains(&value.len()) || !value.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return None;
    }
    Some((label, value))
}

fn ensure_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.chars().count() <= 16 && !name.chars().any(char::is_whitespace),
        "凭据名称需为 1-16 个非空白字符"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_store_intent;

    #[test]
    fn parses_store_intent() {
        assert_eq!(
            parse_store_intent("帮我记住 ftp 的密码是 upload_80894"),
            Some(("ftp".to_owned(), "upload_80894".to_owned()))
        );
        assert_eq!(
            parse_store_intent("存一下校园卡密码 11216050"),
            Some(("校园卡".to_owned(), "11216050".to_owned()))
        );
        assert_eq!(
            parse_store_intent("把 minimax 的 token 记住：sk-abc123"),
            Some(("minimax".to_owned(), "sk-abc123".to_owned()))
        );
        // 没名字的落「默认」。
        assert_eq!(
            parse_store_intent("帮我记住密码是 abc123"),
            Some(("默认".to_owned(), "abc123".to_owned()))
        );
    }

    #[test]
    fn rejects_non_intent_and_questions() {
        // 无意图词：只是陈述，不拦（留给即时使用场景）。
        assert_eq!(parse_store_intent("ftp 的密码是 upload_80894"), None);
        // 问句/无值：不命中。
        assert_eq!(parse_store_intent("帮我记住我的密码是多少来着"), None);
        assert_eq!(parse_store_intent("我忘记密码了"), None);
        // 值太短：不像凭据。
        assert_eq!(parse_store_intent("记住wifi密码12"), None);
    }
}
