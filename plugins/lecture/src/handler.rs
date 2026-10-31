//! QQ 消息拦截：单聊收到课堂录音文件（音频扩展名）时立即下载落盘，
//! 写入待转写队列并即时回执；转写整理在 worker 侧异步完成。

use crate::config::LectureConfig;
use crate::pending::{is_audio_filename, sanitize_name, PendingItem, PENDING_KIND};
use anyhow::{Context, Result};
use ru41::ports::channel::MediaKind;
use ru41::ports::commands::{CommandContext, CommandHandler, Reply};
use std::{future::Future, pin::Pin, time::Duration};

pub struct LectureHandler {
    config: LectureConfig,
    http: reqwest::Client,
}

impl LectureHandler {
    pub fn new(config: LectureConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .expect("lecture http client"),
        }
    }

    async fn intercept(&self, content: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        let audio = ctx
            .attachments
            .iter()
            .find(|m| m.kind == MediaKind::File && is_audio_filename(m.display_name()));
        let Some(audio) = audio else {
            return Ok(None);
        };
        if let Some(note) = ctx.require_private("转写课堂录音", "课堂录音属于个人学习资料")? {
            return Ok(Some(Reply::text(note)));
        }
        let filename = sanitize_name(audio.display_name());
        let raw_dir = self.config.workspace.join(ctx.msg_id).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        let dest = raw_dir.join(&filename);
        // QQ CDN 链接有时效，必须当场下载完。
        let size = self.download(&audio.source, &dest).await?;
        let item = PendingItem {
            file: dest.display().to_string(),
            filename: audio.display_name().to_owned(),
            size,
            channel: ctx.reply_channel.to_owned(),
            target: ctx.target.to_owned(),
            msg_id: ctx.msg_id.to_owned(),
            received_at: ru41::util::now(),
            status: "pending".into(),
            error: None,
            transcript_path: None,
            course: None,
        };
        // put_document(None) insert-or-ignore：同 msg_id 重复投递不覆盖（幂等）。
        ctx.store.put_document(
            PENDING_KIND,
            ctx.msg_id,
            None,
            &serde_json::to_value(&item)?,
        )?;
        let hint = (!content.trim().is_empty())
            .then(|| format!("\n\n> 附言：{}", content.trim()))
            .unwrap_or_default();
        Ok(Some(Reply::text(format!(
            "## 课堂录音已接收\n\n《{}》（{:.1} MB）\n已下载并排队转写，每 {} 分钟一片逐段识别；\
             完成后会按当天课表归入对应课程，自动整理成课堂笔记并通知你。{hint}",
            audio.display_name(),
            size as f64 / 1024.0 / 1024.0,
            self.config.slice_secs / 60,
        ))))
    }

    /// 流式落盘，避免大文件整读进内存。
    async fn download(&self, url: &str, dest: &std::path::Path) -> Result<u64> {
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("下载录音失败：{url}"))?
            .error_for_status()
            .with_context(|| format!("下载录音 HTTP 异常：{url}"))?;
        let mut file = std::fs::File::create(dest)
            .with_context(|| format!("创建文件失败 {}", dest.display()))?;
        let mut size = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .with_context(|| format!("下载录音中断：{url}"))?
        {
            std::io::copy(&mut chunk.as_ref(), &mut file)
                .with_context(|| format!("写入录音失败 {}", dest.display()))?;
            size += chunk.len() as u64;
        }
        Ok(size)
    }
}

impl CommandHandler for LectureHandler {
    /// 不接命令，只拦带音频附件的自然语言/文件消息。
    fn matches(&self, _verb: &str) -> bool {
        false
    }

    fn help(&self) -> &'static str {
        "（自动）单聊发课堂录音文件，自动转写并整理成课堂笔记"
    }

    fn handle<'a>(
        &'a self,
        _arg: &'a str,
        _ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async { Ok(None) })
    }

    fn handle_message<'a>(
        &'a self,
        content: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move { self.intercept(content, ctx).await })
    }
}
