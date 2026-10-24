//! 文本向量化端口：知识库检索（RAG）的 embedding 抽象。
//! 实现侧默认本地模型（adapters/embedding/fastembed），
//! 私人笔记不出服务器即可完成索引与检索。

use anyhow::Result;

/// 文本 → 向量。实现自行处理并发与批大小。
pub trait Embedder: Send + Sync {
    /// 批量向量化；返回顺序与输入一致。
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// 向量维度（建库时校验一致性，换模型必须重建索引）。
    fn dim(&self) -> usize;
    /// 模型标识，写入索引元信息。
    fn model_name(&self) -> &str;
}
