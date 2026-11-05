//! QQ 消息拦截：单聊收到课堂录音文件（音频扩展名）时立即下载落盘，
//! 写入待转写队列并即时回执；转写整理在 worker 侧异步完成。

use crate::config::LectureConfig;
use crate::pending::{PENDING_KIND, PendingItem, is_audio_filename, sanitize_name};
use anyhow::{Context, Result};
use ru41::ports::channel::MediaKind;
use ru41::ports::commands::{CommandContext, CommandHandler, Reply};
use std::{future::Future, pin::Pin, time::Duration};

#[derive(Clone)]
pub struct LectureHandler {
    config: LectureConfig,
    http: reqwest::Client,
}

impl LectureHandler {
    pub fn new(config: LectureConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(600))
                .build()
                .expect("lecture http client"),
        }
    }

    /// 下载 + 入库（插件自己的 runtime 里跑，见 handle_message 的 bridge 说明）。
    /// 返回（文件大小 MB 前的字节数, 前面排队节数）：排队数让回执告诉用户还要等几节。
    #[allow(clippy::too_many_arguments)]
    async fn run_job(
        &self,
        store: std::sync::Arc<dyn ru41::ports::storage::TaskStore>,
        source: String,
        auth: Option<String>,
        display_name: String,
        channel: String,
        target: String,
        msg_id: String,
    ) -> Result<(u64, usize)> {
        let filename = sanitize_name(&display_name);
        let raw_dir = self.config.workspace.join(&msg_id).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        let dest = raw_dir.join(&filename);
        // QQ CDN 链接有时效，必须当场下载完。
        let size = self.download(&source, auth.as_deref(), &dest).await?;
        let item = PendingItem {
            file: dest.display().to_string(),
            filename: display_name,
            size,
            channel,
            target,
            msg_id: msg_id.clone(),
            received_at: ru41::util::now(),
            status: "pending".into(),
            error: None,
            course: None,
        };
        // put_document(None) insert-or-ignore：同 msg_id 重复投递不覆盖（幂等）。
        store.put_document(PENDING_KIND, &msg_id, None, &serde_json::to_value(&item)?)?;
        // 队列位置：除本条外仍未消化完的节数（worker 串行，按这个数报「前面还有几节」）。
        let ahead = store
            .documents(PENDING_KIND, 100)?
            .iter()
            .filter_map(|doc| PendingItem::from_document(doc).ok())
            .filter(|other| {
                other.msg_id != msg_id && other.status != "done" && other.status != "failed"
            })
            .count();
        Ok((size, ahead))
    }

    /// 流式落盘，避免大文件整读进内存。
    /// 连接类失败重试 3 次（2s/6s 退避）：家宽到腾讯云 CDN 段偶发整段劣化，
    /// 单次尝试会把好链接白白判死；HTTP 状态错误不重试（rkey 失效重试也没用）。
    /// auth 是渠道 CDN 认证头值（QQ 要求 `Authorization: QQBot <token>`）。
    async fn download(&self, url: &str, auth: Option<&str>, dest: &std::path::Path) -> Result<u64> {
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2 * attempt as u64 + 1)).await;
            }
            match self.try_download(url, auth, dest).await {
                Ok(size) => return Ok(size),
                Err(err) => {
                    let retriable = err.chain().any(|cause| {
                        cause
                            .downcast_ref::<reqwest::Error>()
                            .is_some_and(|e| e.is_connect() || e.is_timeout())
                    });
                    last_err = Some(err);
                    if !retriable {
                        break;
                    }
                }
            }
        }
        Err(last_err
            .unwrap()
            .context(format!("下载录音失败（已重试 3 次）：{url}")))
    }

    async fn try_download(
        &self,
        url: &str,
        auth: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<u64> {
        let mut request = self.http.get(url);
        if let Some(auth) = auth {
            request = request.header(reqwest::header::AUTHORIZATION, auth);
        }
        let mut response = request
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
        Box::pin(async move {
            let audio = ctx
                .attachments
                .iter()
                .find(|m| m.kind == MediaKind::File && is_audio_filename(m.display_name()))
                .cloned();
            let Some(audio) = audio else {
                return Ok(None);
            };
            if let Some(note) = ctx.require_private("转写课堂录音", "课堂录音属于个人学习资料")?
            {
                return Ok(Some(Reply::text(note)));
            }
            // bridge：插件 cdylib 带独立 tokio 副本，reqwest 的 reactor TLS 跨不过
            // .so 边界，网络下载必须放进插件自己的 runtime；入参全部物化为 owned。
            let worker = self.clone();
            let store = ctx.store.clone();
            let source = audio.source.clone();
            let auth = audio.auth.clone();
            let display_name = audio.display_name().to_owned();
            let channel = ctx.reply_channel.to_owned();
            let target = ctx.target.to_owned();
            let msg_id = ctx.msg_id.to_owned();
            let (size, ahead) = match ru41::plugins::bridge(async move {
                worker
                    .run_job(store, source, auth, display_name, channel, target, msg_id)
                    .await
            })
            .await
            {
                Ok(ok) => ok,
                // 下载失败要回执（链接时效/网络故障用户需要知道并重发），
                // 不能只在服务端日志里留个 handle failed。
                Err(error) => {
                    return Ok(Some(Reply::text(format!(
                        "## 录音接收失败\n\n《{}》下载没成功：{error:#}\n\n\
                         QQ 文件链接有时效，也可能是网络临时故障，请重新发一次。",
                        audio.display_name(),
                    ))));
                }
            };
            let hint = if !content.trim().is_empty() {
                format!("\n\n> 附言：{}", content.trim())
            } else {
                Default::default()
            };
            let queue = if ahead > 0 {
                format!(
                    "\n- 队列：前面还有 {} 节在排队，转写按收到顺序一节一节来",
                    ahead
                )
            } else {
                "\n- 队列：就这一节，马上开始".to_owned()
            };
            Ok(Some(Reply::text(format!(
                "## 课堂录音已接收\n\n《{}》（{:.1} MB）\n已下载并排队转写，每 {} 分钟一片逐段识别；\
                 完成后会按当天课表归入对应课程，自动整理成课堂笔记存进 vault 并通知你。{}{}",
                audio.display_name(),
                size as f64 / 1024.0 / 1024.0,
                self.config.slice_secs / 60,
                queue,
                hint,
            ))))
        })
    }
}
