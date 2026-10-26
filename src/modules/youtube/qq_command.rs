use crate::ports::commands::{CommandContext, CommandHandler, Reply};
use anyhow::Result;
use std::{future::Future, pin::Pin};

use super::{service::prep_video_task, source::YtDlpClient};

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

impl CommandHandler for YoutubeQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(
            verb,
            "预习" | "prep" | "复习" | "review" | "定级" | "level" | "答案" | "answer"
        )
    }

    fn help(&self) -> &'static str {
        "`/预习 <YouTube链接>`：生成该视频的英语预习单；`/复习 <不会的编号>`（如 /复习 2 4，其余记会；全部都会回 /复习 all）；`/定级`：英语水平测试（3 句翻译），结果用于调整预习难度；`/答案`：最近一期看后检测的答案"
    }

    fn handle_command<'a>(
        &'a self,
        verb: &'a str,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if matches!(verb, "复习" | "review") {
                let feedback = if arg.trim().is_empty() { "all" } else { arg };
                return Ok(Some(Reply::text(super::review::apply_feedback(
                    ctx.store, feedback,
                )?)));
            }
            if matches!(verb, "答案" | "answer") {
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
            prep_video_task(
                ctx.store,
                &self.client,
                &video_id,
                &self.interests,
                &self.level,
                self.max_transcript_chars,
                &self.backend,
                Some(&route),
            )
            .await
            .map(Reply::text)
            .map(Some)
        })
    }

    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        self.handle_command("预习", arg, ctx)
    }
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
    fn rejects_non_video_input() {
        assert!(parse_video_id("").is_none());
        assert!(parse_video_id("帮我总结一下今天的课").is_none());
        assert!(parse_video_id("https://example.com/watch?v=short").is_none());
        assert!(parse_video_id("https://www.bilibili.com/video/BV1xx411c7mD").is_none());
    }
}
