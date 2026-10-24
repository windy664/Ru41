//! 本地 embedding：fastembed（ONNX Runtime）跑 bge-small-zh-v1.5。
//! 中文个人笔记场景够用（512 维），模型首次使用时下载到 cache_dir
//! （容器内建议 /data/fastembed，随数据卷持久化）。

use crate::ports::embedding::Embedder;
use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use std::path::PathBuf;
use std::sync::Mutex;

pub const DEFAULT_MODEL: &str = "bge-small-zh-v1.5";
const MODEL_DIM: usize = 512;
/// 单次 embed 调用的批大小；ONNX 对长批次更省调度开销。
const BATCH_SIZE: usize = 32;

pub struct FastembedEmbedder {
    inner: Mutex<TextEmbedding>,
}

impl FastembedEmbedder {
    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        let options = TextInitOptions::new(EmbeddingModel::BGESmallZHV15)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false);
        let inner = TextEmbedding::try_new(options).context("init fastembed bge-small-zh")?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }
}

impl Embedder for FastembedEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut model = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        model
            .embed(texts, Some(BATCH_SIZE))
            .map_err(anyhow::Error::msg)
    }

    fn dim(&self) -> usize {
        MODEL_DIM
    }

    fn model_name(&self) -> &str {
        DEFAULT_MODEL
    }
}
