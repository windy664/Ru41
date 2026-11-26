//! 插件间协作用的转写能力：**听悟优先，失败回落本地**。
//!
//! 2026-11-26 加：课堂录音（QQ 发来的语音文件）本来由 `lecture` 插件处理，
//! 而 lecture 用的是本地 ASR（无说话人分离）。用户要的是「先走听悟，兜底本地」，
//! 于是这里把听悟包装成一个**转写能力**注册进宿主级注册表，
//! `lecture` 的 worker 转写前先查表：命中听悟就用听悟，听悟失败再回自己的本地 ASR。
//!
//! 顺序含义：
//! 1. 听悟（云端，带说话人分离 + 时间戳）；
//! 2. 听悟失败（配额用完 / cookie 过期 / 网络抽风）→ 本地 ASR（只有字）。
//!
//! ⚠️ 网络调用必须过 [`ru41::plugins::bridge`]：插件 cdylib 带独立 tokio 副本，
//! reqwest 的 reactor 跨不过 .so 边界（同 lecture 的注释）。

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use ru41::ports::storage::TaskStore;
use ru41::ports::transcriber::Transcriber;

use crate::client::TingwuClient;
use crate::config::TingwuSection;

/// 听悟转写能力。
pub struct TingwuTranscriber {
    section: TingwuSection,
}

impl Default for TingwuTranscriber {
    fn default() -> Self {
        Self {
            section: TingwuSection::default(),
        }
    }
}

impl TingwuTranscriber {
    pub fn new(section: TingwuSection) -> Self {
        Self { section }
    }
}

impl Transcriber for TingwuTranscriber {
    fn id(&self) -> &str {
        "tingwu"
    }

    fn transcribe<'a>(
        &'a self,
        store: Arc<dyn TaskStore>,
        path: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let section = self.section.clone();
        let path = path.to_path_buf();
        Box::pin(async move {
            // cookie 从凭据托管库取（真值不进仓库、不进模型上下文）。
            let cookie = crate::cli::cookie_from_store(store.as_ref(), section.credential())
                .context("取听悟 cookie 失败")?;
            let client = TingwuClient::new(&cookie, &section).context("建听悟客户端失败")?;
            // 网络调用在插件自己的 runtime 里跑（跨 .so 边界的 reactor 问题）。
            ru41::plugins::bridge(async move {
                match client.transcribe(&path).await {
                    Ok(transcript) => Ok(transcript.full_text()),
                    Err(error) => {
                        eprintln!("[tingwu] 听悟转写失败（{error}），回落本地 ASR");
                        // 兜底：本地 paraformer / whisper.cpp（只有字，没有说话人分离）。
                        let local = client.transcribe_local(&path).await?;
                        Ok(local.full_text())
                    }
                }
            })
            .await
        })
    }
}
