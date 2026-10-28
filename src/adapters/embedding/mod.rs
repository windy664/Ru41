//! Embedding 适配器：ports::embedding::Embedder 的实现。
//! fastembed/ONNX 只在 `local-embed` feature 下编译——它拖 native-tls(openssl)
//! 与 ort 运行时，musl 静态构建（生产部署路径）带不动；未启用时用同签名占位，
//! 构造即报错，知识库链路显式失败而不是编译失败。

#[cfg(feature = "local-embed")]
pub mod fastembed;

#[cfg(not(feature = "local-embed"))]
pub mod fastembed {
    //! `--features local-embed` 未启用时的占位：保持类型与构造签名不变，
    //! 让调用方代码零分叉；任何使用都返回明确错误。
    use crate::ports::embedding::Embedder;
    use anyhow::{Result, bail};
    use std::path::PathBuf;

    pub struct FastembedEmbedder;

    impl FastembedEmbedder {
        pub fn new(_cache_dir: PathBuf) -> Result<Self> {
            bail!(
                "本二进制未编译 local-embed feature（musl 静态构建不内置 ONNX/openssl）；\
                 知识库需要本地 embedding 时用 --features local-embed 重编"
            )
        }
    }

    impl Embedder for FastembedEmbedder {
        fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            bail!("local-embed feature 未启用，embedding 不可用")
        }
        fn dim(&self) -> usize {
            0
        }
        fn model_name(&self) -> &str {
            "unavailable"
        }
    }
}
