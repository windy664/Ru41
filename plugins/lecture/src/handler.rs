//! QQ 消息拦截：单聊收到课堂录音文件（音频扩展名）时立即下载落盘，
//! 写入待转写队列并即时回执；转写整理在 worker 侧异步完成。

use crate::config::LectureConfig;
use crate::dns;
use crate::pending::{
    PENDING_KIND, PendingItem, is_audio_filename, recorded_time_of, sanitize_name,
};
use crate::service::match_course;
use anyhow::{Context, Result};
use ru41::domain::todo::TodoOptions;
use ru41::ports::channel::MediaKind;
use ru41::ports::commands::{CommandContext, CommandHandler, Reply};
use std::{future::Future, net::SocketAddr, pin::Pin, time::Duration};

#[derive(Clone)]
pub struct LectureHandler {
    config: LectureConfig,
    /// 时区口径：与 worker 共用，回执里匹配出的课程名才和最终归档一致。
    options: TodoOptions,
    http: reqwest::Client,
}

/// 下载客户端的统一参数（超时），普通客户端与「固定 IP」客户端共用。
fn http_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
}

impl LectureHandler {
    pub fn new(config: LectureConfig, options: TodoOptions) -> Self {
        Self {
            config,
            options,
            http: http_builder().build().expect("lecture http client"),
        }
    }

    /// 下载 + 入库（插件自己的 runtime 里跑，见 handle_message 的 bridge 说明）。
    /// 返回（文件大小字节数, 前面排队节数, 收到时刻）：
    /// 排队数让回执告诉用户还要等几节；收到时刻用于回执侧匹配课程名。
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
    ) -> Result<(u64, usize, i64)> {
        let filename = sanitize_name(&display_name);
        let raw_dir = self.config.workspace.join(&msg_id).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        let dest = raw_dir.join(&filename);
        // QQ CDN 链接有时效，必须当场下载完。
        let size = self.download(&source, auth.as_deref(), &dest).await?;
        let received_at = ru41::util::now();
        let item = PendingItem {
            file: dest.display().to_string(),
            filename: display_name,
            size,
            channel,
            target,
            msg_id: msg_id.clone(),
            received_at,
            status: "pending".into(),
            error: None,
            course: None,
            transcript: None,
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
        Ok((size, ahead, received_at))
    }

    /// 流式落盘，避免大文件整读进内存。
    /// 连接类失败重试 3 次（2s/6s 退避）：家宽到腾讯云 CDN 段偶发整段劣化，
    /// 单次尝试会把好链接白白判死；HTTP 状态错误不重试（rkey 失效重试也没用）。
    /// 三次仍连不上，多半不是链路抖动而是 DNS 给的 IP 不通（被污染的边缘），
    /// 这时改用 DoH 解析出的 IP 重连。
    /// auth 是渠道 CDN 认证头值（QQ 要求 `Authorization: QQBot <token>`）。
    async fn download(&self, url: &str, auth: Option<&str>, dest: &std::path::Path) -> Result<u64> {
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2 * attempt as u64 + 1)).await;
            }
            match self.try_download(&self.http, url, auth, dest).await {
                Ok(size) => return Ok(size),
                Err(err) => {
                    let retriable = is_connect_failure(&err);
                    last_err = Some(err);
                    if !retriable {
                        break;
                    }
                }
            }
        }
        let err = last_err.unwrap();
        // 只有连接类失败才值得换 IP；HTTP 4xx/5xx 换 DNS 也救不回来。
        if !is_connect_failure(&err) {
            return Err(err.context(format!("下载录音失败：{url}")));
        }
        let fallback = self.download_via_doh(url, auth, dest).await;
        match fallback {
            Ok(size) => Ok(size),
            Err((doh_err, original)) => Err(original.context(format!(
                "下载录音失败（已重试 3 次；DoH 解析重连也失败：{doh_err:#}）：{url}"
            ))),
        }
    }

    /// 系统 DNS 给的 IP 连不上时，用 DoH 解析出的候选 IP 逐个重连。
    async fn download_via_doh(
        &self,
        url: &str,
        auth: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<u64, (anyhow::Error, anyhow::Error)> {
        let parsed = match reqwest::Url::parse(url) {
            Ok(parsed) => parsed,
            Err(err) => return Err((err.into(), anyhow::anyhow!("录音链接无法解析"))),
        };
        let Some(host) = parsed.host_str().map(str::to_string) else {
            return Err((
                anyhow::anyhow!("录音链接缺少主机名"),
                anyhow::anyhow!("录音链接缺少主机名"),
            ));
        };
        let port = parsed.port_or_known_default().unwrap_or(443);
        let ips = dns::resolve_a(&host).await;
        let mut last: Option<anyhow::Error> = None;
        for ip in ips.into_iter().take(4) {
            let client = match http_builder()
                .resolve(&host, SocketAddr::new(ip, port))
                .build()
            {
                Ok(client) => client,
                Err(err) => {
                    last = Some(err.into());
                    continue;
                }
            };
            match self.try_download(&client, url, auth, dest).await {
                Ok(size) => return Ok(size),
                Err(err) => last = Some(err),
            }
        }
        Err((
            last.unwrap_or_else(|| anyhow::anyhow!("DoH 没有解析到可用的 A 记录")),
            anyhow::anyhow!("系统 DNS 解析到的地址都连不上"),
        ))
    }

    async fn try_download(
        &self,
        client: &reqwest::Client,
        url: &str,
        auth: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<u64> {
        let mut request = client.get(url);
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

/// 连接类失败（连不上/超时）：换 IP 有意义。
fn is_connect_failure(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|e| e.is_connect() || e.is_timeout())
    })
}

impl CommandHandler for LectureHandler {
    /// 不接命令，只拦带音频附件的自然语言/文件消息。
    fn matches(&self, _verb: &str) -> bool {
        false
    }

    fn help(&self) -> &'static str {
        "（自动）单聊发来一节课的音频文件，自动转写并整理成课堂笔记"
    }

    fn help_section(&self) -> &'static str {
        "课程与学习"
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
            if let Some(note) = ctx.require_private("转写课堂内容", "课堂内容属于个人学习资料")?
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
            let (size, ahead, received_at) = match ru41::plugins::bridge(async move {
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
                        "## 这份文件没收到\n\n《{}》下载没成功：{error:#}\n\n\
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
            // 回执的主语是「这节课」，不是「一段音频」：课表能对上就直接点名课程，
            // 对不上才说课程未知、笔记先放收件箱。匹配逻辑与 worker 共用，
            // 所以这里说的课程就是最后写进 Vault 的那门课。
            let recorded_at = recorded_time_of(&audio.display_name(), received_at);
            let course = match_course(&ctx.store, self.options, &self.config, recorded_at);
            let matched = course != self.config.fallback_course;
            let headline = if matched {
                format!("## {} 这节课已结束", course)
            } else {
                "## 这节课已结束".to_owned()
            };
            let action = if matched {
                "开始分析这节课并整理成课堂笔记，完成后写进 Vault 并通知你。"
            } else {
                "开始整理课堂笔记；课表里没对上这节课的时间，笔记先放收件箱，等你确认归到哪门课。"
            };
            let queue = if ahead > 0 {
                format!("\n- 队列：前面还有 {} 节，按收到的顺序来", ahead)
            } else {
                "\n- 队列：就这一节，不用等".to_owned()
            };
            Ok(Some(Reply::text(format!(
                "{headline}\n\n{action}\n《{}》（{:.1} MB），分段识别中（每段约 {} 分钟）{}{}",
                audio.display_name(),
                size as f64 / 1024.0 / 1024.0,
                self.config.slice_secs / 60,
                queue,
                hint,
            ))))
        })
    }
}
