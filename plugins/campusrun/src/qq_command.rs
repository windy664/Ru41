//! QQ 命令 `/校园跑`：查进度/记录/风控，看规则面，预演，以及（经确认后）跑一次。
//!
//! 「跑一次」分两步：先 `/校园跑 跑 3 圈` → 出预演摘要 + 确认口令；
//! 再 `/校园跑 确认 <口令>` → 才真的提交。提交在后台跑（不堵 QQ 入站线程），
//! 完成后用通知通道把结果发回原会话。

use crate::client::CampusrunClient;
use crate::config::CampusrunSection;
use crate::service::fetch_markdown;
use crate::submit::{RunIntent, make_token};
use anyhow::Result;
use ru41::ports::channel::NotifyChannel;
use ru41::ports::commands::{CommandContext, Reply, SimpleCommand};
use std::sync::Arc;

/// 校园跑命令。
pub struct CampusrunQqCommand {
    section: CampusrunSection,
    /// 主动通知通道（用于把提交结果发回原会话；没有就只回执）。
    notifier: Option<Arc<dyn NotifyChannel>>,
}

impl CampusrunQqCommand {
    pub fn new(section: CampusrunSection, notifier: Option<Arc<dyn NotifyChannel>>) -> Self {
        Self { section, notifier }
    }
}

/// 用户想要什么。
#[derive(Debug)]
enum Action {
    Status,
    Records,
    Cheat,
    All,
    Rules,
    /// 预演（不提交）。
    Plan(RunIntent),
    /// 登记一条待确认的提交（不提交）。
    Run(RunIntent),
    /// 确认并提交。
    Confirm(String),
}

impl SimpleCommand for CampusrunQqCommand {
    fn verbs(&self) -> &'static [&'static str] {
        &["校园跑", "跑步", "campusrun"]
    }

    fn help(&self) -> &'static str {
        "`/校园跑 [进度|记录|状态|规则|预演 N圈|跑 N圈|确认 口令]`：查校园跑进度/记录/风控，看规则与必过点，预演或跑一次（需绑定身份，仅单聊）"
    }

    fn help_section(&self) -> &'static str {
        "校园生活"
    }

    async fn run(&self, arg: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        let Some(action) = parse_action(arg) else {
            return Ok(None);
        };
        if let Some(reply) = ctx.require_private("查询校园跑", "校园跑记录属于个人信息")?
        {
            return Ok(Some(reply.into()));
        }
        let section = self.section.clone();
        let client = CampusrunClient::new(&section).map_err(|error| anyhow::anyhow!("{error}"))?;
        match action {
            Action::Status => {
                let text = fetch_markdown(&client, "status", 5)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                Ok(Some(Reply::text(text)))
            }
            Action::Records => {
                let text = fetch_markdown(&client, "records", 5)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                Ok(Some(Reply::text(text)))
            }
            Action::Cheat => {
                let text = fetch_markdown(&client, "cheat", 5)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                Ok(Some(Reply::text(text)))
            }
            Action::All => {
                let text = fetch_markdown(&client, "all", 5)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                Ok(Some(Reply::text(text)))
            }
            Action::Rules => {
                let text = crate::service::fetch_rules(&client)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                Ok(Some(Reply::text(text)))
            }
            Action::Plan(intent) => {
                let (summary, plan, _params) =
                    crate::service::fetch_plan(&client, &section, &intent)
                        .await
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                let tail = if plan.blocked.unwrap_or(false) {
                    "预演被拦下，**不能提交**。"
                } else {
                    "预演通过，可以提交。"
                };
                Ok(Some(Reply::text(format!("{summary}\n\n{tail}"))))
            }
            Action::Run(intent) => {
                if !section.allow_submit() {
                    // 没开提交就只给预演，绝不静默提交。
                    let (summary, _, _params) =
                        crate::service::fetch_plan(&client, &section, &intent)
                            .await
                            .map_err(|error| anyhow::anyhow!("{error}"))?;
                    return Ok(Some(Reply::text(format!(
                        "{summary}\n\n提交功能没开（[sources.campusrun] 里 allow_submit = true 才开），\
                         刚才只做了预演，没有提交任何记录。"
                    ))));
                }
                let (summary, plan, params) =
                    crate::service::fetch_plan(&client, &section, &intent)
                        .await
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                if plan.blocked.unwrap_or(false) {
                    return Ok(Some(Reply::text(format!(
                        "预演自检没过，没法提交：{}",
                        plan.issue_summary.unwrap_or_default()
                    ))));
                }
                // 用**预演那一套**参数登记：服务端每次请求都会重新随机起跑时刻，
                // 再取一次参数，用户看到的起跑时刻就会和真交上去的那条对不上。
                let now = ru41::util::now();
                let token = make_token(ctx.msg_id, now);
                crate::service::register_pending(
                    &Arc::clone(ctx.store),
                    &params,
                    &summary,
                    &token,
                    now,
                )?;
                Ok(Some(Reply::text(format!(
                    "{summary}\n\n还没提交。确认要提交就发：\n`/校园跑 确认 {token}`\n\
                     （待确认有效期 {} 分钟；提交后 {} 秒内不能再跑）",
                    section.confirm_ttl_secs() / 60,
                    section.silence_secs()
                ))))
            }
            Action::Confirm(token) => {
                if !section.allow_submit() {
                    return Ok(Some(Reply::text(
                        "提交功能没开（[sources.campusrun] 里 allow_submit = true 才开），没有提交任何记录。",
                    )));
                }
                // 提交要十几秒，放后台跑，别堵 QQ 入站线程（今天刚踩过这个坑）。
                // 注意：ctx.store 是 &Arc，.clone() 会克隆引用而不是 Arc（借用逃逸）。
                // 这里要的是拥有所有权的 Arc，bridge 的 future 必须是 'static。
                let store = Arc::clone(ctx.store);
                let section = self.section.clone();
                let notifier = self.notifier.clone();
                // target/msg_id 都是 &str，克隆引用会借用逃逸——要拥有所有权。
                let target = ctx.target.to_owned();
                let now = ru41::util::now();
                drop(ru41::plugins::bridge(async move {
                    let client = match CampusrunClient::new(&section) {
                        Ok(client) => client,
                        Err(error) => {
                            eprintln!("[campusrun] 客户端构造失败：{error}");
                            return Ok::<(), anyhow::Error>(());
                        }
                    };
                    let outcome =
                        crate::service::confirm_and_submit(&store, &client, &section, &token, now)
                            .await;
                    let text = match outcome {
                        Ok(outcome) => outcome.text(),
                        Err(error) => format!("❌ 校园跑提交失败：{error:#}"),
                    };
                    eprintln!("[campusrun] 提交结果：{text}");
                    if let Some(notifier) = notifier
                        && let Err(error) = notifier.send(&target, None, &text).await
                    {
                        eprintln!("[campusrun] 结果通知失败：{error:#}");
                    }
                    Ok::<(), anyhow::Error>(())
                }));
                Ok(Some(Reply::text(
                    "提交中（要十几秒），完成后把结果发给你。",
                )))
            }
        }
    }
}

/// 解析中文参数。
fn parse_action(arg: &str) -> Option<Action> {
    let text = arg.trim();
    if text.is_empty() {
        return Some(Action::Status);
    }
    // 确认 <token>
    if let Some(rest) = text.strip_prefix("确认").map(str::trim) {
        let token = rest
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches('`')
            .to_owned();
        if !token.is_empty() {
            return Some(Action::Confirm(token));
        }
    }
    if text.contains("规则") || text.contains("必过点") || text.contains("打卡点") {
        return Some(Action::Rules);
    }
    if text.contains("预演") || text.contains("试跑") || text.contains("看看能不能跑") {
        return Some(Action::Plan(intent_from_text(text)?));
    }
    if text.contains("跑") {
        return Some(Action::Run(intent_from_text(text)?));
    }
    if text.contains("风控") || text.contains("违规") {
        return Some(Action::Cheat);
    }
    if text.contains("记录") || text.contains("跑了哪几次") {
        return Some(Action::Records);
    }
    if text.contains("全部") || text.contains("总览") {
        return Some(Action::All);
    }
    if text.contains("进度") || text.contains("多少公里") || text.contains("成绩") {
        return Some(Action::Status);
    }
    None
}

/// 从中文里读出「跑什么」：圈数或公里数。
fn intent_from_text(text: &str) -> Option<RunIntent> {
    let number = extract_number(text)?;
    if text.contains("公里") || text.contains("km") || text.contains("KM") {
        return Some(RunIntent::DistanceKm { km: number });
    }
    // 其余按圈数（默认第 1 道）。
    Some(RunIntent::Laps {
        laps: number,
        lane: 1,
    })
}

/// 取一段文本里的第一个数字（阿拉伯数字或中文数字）。
fn extract_number(text: &str) -> Option<f64> {
    // 阿拉伯数字（含小数）
    let bytes = text.as_bytes();
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_digit() {
            continue;
        }
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            end += 1;
        }
        if let Ok(value) = text[start..end].parse::<f64>() {
            return Some(value);
        }
    }
    chinese_number(text)
}

/// 中文数字（一 ~ 九十九）。
fn chinese_number(text: &str) -> Option<f64> {
    let digit = |ch: char| -> Option<f64> {
        Some(match ch {
            '零' | '〇' => 0.0,
            '一' | '壹' => 1.0,
            '二' | '两' | '贰' => 2.0,
            '三' | '叁' => 3.0,
            '四' | '肆' => 4.0,
            '五' | '伍' => 5.0,
            '六' | '陆' => 6.0,
            '七' | '柒' => 7.0,
            '八' | '捌' => 8.0,
            '九' | '玖' => 9.0,
            '十' | '拾' => 10.0,
            _ => return None,
        })
    };
    let chars: Vec<char> = text.chars().filter(|ch| digit(*ch).is_some()).collect();
    if chars.is_empty() {
        return None;
    }
    if chars.len() == 1 {
        return digit(chars[0]);
    }
    // 十X / X十 / X十Y
    let mut total = 0.0;
    let mut index = 0;
    while index < chars.len() {
        let current = digit(chars[index])?;
        if current == 10.0 {
            let lead = if index == 0 { 1.0 } else { total };
            total = lead * 10.0;
            if index + 1 < chars.len() {
                total += digit(chars[index + 1])?;
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        total += current;
        index += 1;
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_actions() {
        assert!(matches!(parse_action(""), Some(Action::Status)));
        assert!(matches!(parse_action("进度"), Some(Action::Status)));
        assert!(matches!(parse_action("记录"), Some(Action::Records)));
        assert!(matches!(parse_action("风控"), Some(Action::Cheat)));
        assert!(matches!(parse_action("规则"), Some(Action::Rules)));
        assert!(matches!(
            parse_action("确认 abc123"),
            Some(Action::Confirm(_))
        ));
        assert!(parse_action("随便聊聊").is_none());
    }

    #[test]
    fn extracts_laps_and_km() {
        match parse_action("跑 3 圈") {
            Some(Action::Run(RunIntent::Laps { laps, lane })) => {
                assert_eq!(laps, 3.0);
                assert_eq!(lane, 1);
            }
            other => panic!("{other:?}"),
        }
        match parse_action("预演 1.2 公里") {
            Some(Action::Plan(RunIntent::DistanceKm { km })) => assert!((km - 1.2).abs() < 0.001),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_chinese_numbers() {
        assert_eq!(extract_number("跑三圈"), Some(3.0));
        assert_eq!(extract_number("跑十圈"), Some(10.0));
        assert_eq!(extract_number("跑十二圈"), Some(12.0));
        assert_eq!(extract_number("跑 5 圈"), Some(5.0));
        assert_eq!(extract_number("跑两圈"), Some(2.0));
        assert_eq!(extract_number("跑"), None);
    }
}
