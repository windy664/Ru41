use anyhow::Result;
use ru41::ports::commands::{CommandContext, Reply, SimpleCommand};

use crate::{service::prep_video_task, source::YtDlpClient};

/// QQ 预习命令：`/预习 <YouTube链接>` 按需生成英语预习单并推送当前会话。
/// 由装配层注册，QQ 渠道无需认识 youtube 模块。
pub struct YoutubeQqCommand {
    client: YtDlpClient,
    interests: String,
    level: String,
    max_transcript_chars: usize,
    backend: String,
}

impl YoutubeQqCommand {
    pub fn new(
        client: YtDlpClient,
        interests: String,
        level: String,
        max_transcript_chars: usize,
        backend: String,
    ) -> Self {
        Self {
            client,
            interests,
            level,
            max_transcript_chars,
            backend,
        }
    }
}

impl SimpleCommand for YoutubeQqCommand {
    fn verbs(&self) -> &'static [&'static str] {
        &[
            "英语", "english", "复习", "review", "定级", "level", "答案", "answer", "学习", "study",
        ]
    }

    fn help(&self) -> &'static str {
        "`/英语 <YouTube链接>`：生成该视频的学习单；复习、答案与定级按学习单提示操作"
    }

    fn help_section(&self) -> &'static str {
        "信息与学习"
    }

    async fn run_command(
        &self,
        verb: &str,
        arg: &str,
        ctx: &CommandContext<'_>,
    ) -> Result<Option<Reply>> {
        if matches!(verb, "复习" | "review") {
            // 只认编号/all；「复习 高数」这类普通请求继续交给 Agent。
            if !is_review_feedback(arg) {
                return Ok(None);
            }
            let feedback = if arg.trim().is_empty() { "all" } else { arg };
            return Ok(Some(Reply::text(super::review::apply_feedback(
                ctx.store, feedback,
            )?)));
        }
        if matches!(verb, "学习" | "study") {
            if !arg.trim().is_empty() {
                return Ok(None);
            }
            return Ok(Some(Reply::text(super::review::latest_sheet(ctx.store)?)));
        }
        if matches!(verb, "答案" | "answer") {
            if !arg.trim().is_empty() {
                return Ok(None);
            }
            return Ok(Some(Reply::text(super::review::latest_answers(ctx.store)?)));
        }
        if matches!(verb, "定级" | "level") {
            let route = ctx.reply_route();
            let text = if arg.trim().is_empty() {
                super::review::place_start(ctx.store, &self.backend, Some(&route)).await?
            } else {
                super::review::place_submit(ctx.store, arg, &self.backend, Some(&route)).await?
            };
            return Ok(Some(Reply::text(text)));
        }
        // 非视频链接不拦截，消息继续走 Agent 通道。
        let Some(video_id) = parse_video_id(arg) else {
            return Ok(None);
        };
        let route = ctx.reply_route();
        // ⚠️ 触网（yt-dlp）必须过 bridge：插件 tokio reactor 与宿主不通用。
        let store = ctx.store.clone();
        let client = self.client.clone();
        let interests = self.interests.clone();
        let level = self.level.clone();
        let backend = self.backend.clone();
        let max_transcript_chars = self.max_transcript_chars;
        ru41::plugins::bridge(async move {
            prep_video_task(
                &store,
                &client,
                &video_id,
                &interests,
                &level,
                max_transcript_chars,
                &backend,
                Some(&route),
            )
            .await
        })
        .await
        .map(Reply::text)
        .map(Some)
    }

    async fn run(&self, arg: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        self.run_command("预习", arg, ctx).await
    }
}

/// 复习反馈只接受空（全部都会）、all、或编号列表；避免吞掉普通学习问题。
fn is_review_feedback(arg: &str) -> bool {
    let text = arg.trim();
    text.is_empty()
        || text.eq_ignore_ascii_case("all")
        || text
            .split(|c: char| c.is_whitespace() || matches!(c, ',' | '，' | '、'))
            .filter(|token| !token.is_empty())
            .all(|token| token.chars().all(|c| c.is_ascii_digit()))
}

/// 从 watch / youtu.be / shorts / live 链接或裸 ID 提取 11 位视频 ID。
pub fn parse_video_id(input: &str) -> Option<String> {
    let input = input.trim();
    if is_video_id(input) {
        return Some(input.to_owned());
    }
    for marker in ["v=", "/shorts/", "/live/", "youtu.be/"] {
        let Some(index) = input.find(marker) else {
            continue;
        };
        let rest = &input[index + marker.len()..];
        let candidate: String = rest.chars().take(11).collect();
        if is_video_id(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_video_id(s: &str) -> bool {
    s.len() == 11
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_link_shapes() {
        assert_eq!(
            parse_video_id("https://www.youtube.com/watch?v=LoLYw--s-5w").as_deref(),
            Some("LoLYw--s-5w")
        );
        assert_eq!(
            parse_video_id("https://www.youtube.com/watch?v=LoLYw--s-5w&t=30s").as_deref(),
            Some("LoLYw--s-5w")
        );
        assert_eq!(
            parse_video_id("https://youtu.be/LoLYw--s-5w").as_deref(),
            Some("LoLYw--s-5w")
        );
        assert_eq!(
            parse_video_id("https://www.youtube.com/shorts/LoLYw--s-5w").as_deref(),
            Some("LoLYw--s-5w")
        );
        assert_eq!(
            parse_video_id("LoLYw--s-5w").as_deref(),
            Some("LoLYw--s-5w")
        );
    }

    #[test]
    fn review_feedback_does_not_swallow_study_questions() {
        assert!(is_review_feedback(""));
        assert!(is_review_feedback("all"));
        assert!(is_review_feedback("2 4"));
        assert!(is_review_feedback("2，4"));
        assert!(!is_review_feedback("高数"));
        assert!(!is_review_feedback("2 怎么背"));
    }

    #[test]
    fn rejects_non_video_input() {
        assert!(parse_video_id("").is_none());
        assert!(parse_video_id("帮我总结一下今天的课").is_none());
        assert!(parse_video_id("https://example.com/watch?v=short").is_none());
        assert!(parse_video_id("https://www.bilibili.com/video/BV1xx411c7mD").is_none());
    }
}
