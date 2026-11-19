//! 长期规划拆解的服务层：扫 Vault → 入队 agent 任务 → 轮询 → 写回文档。
//!
//! 与 `obsidian-organize` 同一套安全模型：**agent 只出 JSON，落盘由程序执行**。
//! 跨轮状态存在 `records` 里（kind `plan.decompose`），插件的 tracker 是无状态单轮，
//! 所以每轮都要先读状态再决定做什么。

use std::sync::Arc;

use anyhow::{Context, Result};
use ru41::domain::plan::{self, LongTermPlan};
use ru41::ports::storage::TaskStore;
use ru41::ports::vault::Vault;

use crate::decompose::{decompose_prompt, decompose_request_key};

/// 跨轮状态在 records 里的 kind。
pub const DECOMPOSE_STATE_KIND: &str = "plan.decompose";
/// 状态文档的 id（单条，全局一份）。
pub const DECOMPOSE_STATE_ID: &str = "pending";

/// 运行时配置。
#[derive(Debug, Clone)]
pub struct DecomposeConfig {
    /// Vault 内规划文档目录（相对 vault 根）。
    pub plan_dir: String,
    /// agent 底座 id。
    pub backend: String,
    /// 单轮轮询间隔（秒）。
    pub interval_secs: u64,
}

impl Default for DecomposeConfig {
    fn default() -> Self {
        Self {
            plan_dir: plan::PLAN_DOC_DIR.to_owned(),
            backend: "codex".to_owned(),
            interval_secs: 300,
        }
    }
}

/// 跨轮状态：正在等哪个任务的拆解结果。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecomposePending {
    /// Vault 内相对路径。
    pub doc: String,
    /// 文档版本（commit sha 或内容哈希）—— 变了就不复用旧结果。
    pub sha: String,
    /// agent 任务 id。
    pub task_id: i64,
}

/// 一份待拆解的规划文档。
#[derive(Debug, Clone)]
pub struct PlanDoc {
    /// Vault 内相对路径。
    pub relative: String,
    /// 文档内容。
    pub content: String,
    /// 解析结果。
    pub plan: LongTermPlan,
}

/// 单轮报告。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DecomposeReport {
    /// 找到的待拆解文档数。
    pub pending_docs: usize,
    /// 本轮入队的任务数。
    pub started: usize,
    /// 本轮成功写回的文档数。
    pub applied: usize,
    /// 失败信息（文档 → 原因）。
    pub failed: Vec<String>,
}

/// planner 插件的配置段（`plugins.d/planner/config.toml` 里的 `[planner]`）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PlannerSection {
    /// Vault 内规划文档目录（相对 vault 根）。
    #[serde(default = "default_plan_dir")]
    pub plan_dir: String,
    /// 轮询间隔（秒）。
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
}

fn default_plan_dir() -> String {
    plan::PLAN_DOC_DIR.to_owned()
}
fn default_interval_secs() -> u64 {
    300
}

impl Default for PlannerSection {
    fn default() -> Self {
        Self {
            plan_dir: default_plan_dir(),
            interval_secs: default_interval_secs(),
        }
    }
}

/// 长期规划拆解服务。
#[derive(Clone)]
pub struct PlanDecomposeService {
    store: Arc<dyn TaskStore>,
    vault: Arc<dyn Vault>,
    config: DecomposeConfig,
}

impl PlanDecomposeService {
    pub fn new(store: Arc<dyn TaskStore>, vault: Arc<dyn Vault>, config: DecomposeConfig) -> Self {
        Self {
            store,
            vault,
            config,
        }
    }

    /// 扫描 Vault 规划目录，找出「有里程碑但没 step」的文档。
    pub fn scan(&self) -> Result<Vec<PlanDoc>> {
        let head = self.vault.remote_head().unwrap_or_else(|_| "HEAD".to_owned());
        let prefix = format!("{}/", self.config.plan_dir);
        let files = self
            .vault
            .list_files(&head, &prefix)
            .context("列规划目录失败")?;
        let mut out = Vec::new();
        for file in files {
            if !file.ends_with(".md") {
                continue;
            }
            let Some(content) = self.vault.read_file(&file).context("读规划文档失败")? else {
                continue;
            };
            let Ok(plan) = plan::parse_plan(&content) else {
                continue; // 不是规划文档（缺 frontmatter 等）→ 跳过
            };
            if plan.needs_llm_steps().is_empty() {
                continue; // 已拆解完
            }
            out.push(PlanDoc {
                relative: file,
                content,
                plan,
            });
        }
        out.sort_by(|a, b| a.relative.cmp(&b.relative));
        Ok(out)
    }

    /// 为一份文档入队拆解任务（幂等：同一文档同一版本只入队一次）。
    pub fn start(&self, doc: &PlanDoc, sha: &str) -> Result<i64> {
        let prompt = decompose_prompt(&doc.plan);
        let key = decompose_request_key(&doc.relative, sha);
        let queued = ru41::app::task::TaskService::new(self.store.clone())
            .enqueue_for(&key, &prompt, &self.config.backend)
            .context("入队拆解任务失败")?;
        Ok(queued.task.id)
    }

    /// 推进一轮：先看有没有在等结果的，有就处理；没有就扫描并入队一个。
    pub fn poll_once(&self) -> Result<DecomposeReport> {
        let mut report = DecomposeReport {
            pending_docs: 0,
            started: 0,
            applied: 0,
            failed: Vec::new(),
        };

        // 1) 有在等的任务吗？
        if let Some(pending) = self.pending()? {
            match self.advance(&pending)? {
                Advance::Applied => {
                    report.applied += 1;
                }
                Advance::Waiting => {}
                Advance::Failed(reason) => {
                    report.failed.push(format!("{}: {reason}", pending.doc));
                    self.clear_pending()?;
                }
            }
            return Ok(report);
        }

        // 2) 扫描待拆解文档
        let docs = self.scan()?;
        report.pending_docs = docs.len();
        let Some(doc) = docs.into_iter().next() else {
            return Ok(report);
        };
        let sha = self.doc_sha(&doc)?;
        match self.start(&doc, &sha) {
            Ok(task_id) => {
                self.set_pending(&DecomposePending {
                    doc: doc.relative.clone(),
                    sha,
                    task_id,
                })?;
                report.started += 1;
            }
            Err(error) => report.failed.push(format!("{}: {error:#}", doc.relative)),
        }
        Ok(report)
    }

    /// 处理在等的任务：成功 → 写回文档；失败 → 记录。
    fn advance(&self, pending: &DecomposePending) -> Result<Advance> {
        let Some(task) = self.store.get(pending.task_id)? else {
            return Ok(Advance::Failed("任务不存在".into()));
        };
        use ru41::domain::TaskStatus;
        match task.status {
            TaskStatus::Queued | TaskStatus::Running | TaskStatus::RetryScheduled => {
                Ok(Advance::Waiting)
            }
            TaskStatus::Failed | TaskStatus::Cancelled => Ok(Advance::Failed(
                task.last_error
                    .clone()
                    .unwrap_or_else(|| "任务失败".to_owned()),
            )),
            TaskStatus::Succeeded => {
                let Some(result) = task.result.as_ref() else {
                    return Ok(Advance::Failed("任务成功但没有结果".into()));
                };
                let Some(content) = self.vault.read_file(&pending.doc).context("读规划文档失败")? else {
                    return Ok(Advance::Failed("规划文档已不存在".into()));
                };
                let plan = plan::parse_plan(&content).map_err(|e| anyhow::anyhow!("解析规划文档失败: {e}"))?;
                let expected = plan.needs_llm_steps();
                let steps = match plan::extract_steps(&result.summary, &expected) {
                    Ok(s) => s,
                    Err(reason) => return Ok(Advance::Failed(format!("拆解结果不合格: {reason:#}"))),
                };
                let updated = plan::apply_steps(&content, &steps).map_err(|e| anyhow::anyhow!("写回 step 失败: {e}"))?;
                self.vault
                    .write_file(&pending.doc, &updated)
                    .context("写回规划文档失败")?;
                self.vault
                    .commit_all(&format!("[ru41] 拆解长期规划 {}", pending.doc))
                    .context("提交规划文档失败")?;
                Ok(Advance::Applied)
            }
        }
    }

    /// 文档版本标识：内容哈希（Vault 提交 sha 会随无关提交变，内容哈希更稳）。
    fn doc_sha(&self, doc: &PlanDoc) -> Result<String> {
        Ok(format!("{:x}", sha_like(&doc.content)))
    }

    fn pending(&self) -> Result<Option<DecomposePending>> {
        let Some(document) = self.store.document(DECOMPOSE_STATE_KIND, DECOMPOSE_STATE_ID)? else {
            return Ok(None);
        };
        let pending: DecomposePending =
            serde_json::from_value(document.value).context("拆解状态解析失败")?;
        Ok(Some(pending))
    }

    fn set_pending(&self, pending: &DecomposePending) -> Result<()> {
        let value = serde_json::to_value(pending).context("序列化拆解状态失败")?;
        self.store
            .put_document(DECOMPOSE_STATE_KIND, DECOMPOSE_STATE_ID, None, &value)
            .context("保存拆解状态失败")?;
        Ok(())
    }

    fn clear_pending(&self) -> Result<()> {
        self.store
            .put_document(
                DECOMPOSE_STATE_KIND,
                DECOMPOSE_STATE_ID,
                None,
                &serde_json::Value::Null,
            )
            .context("清除拆解状态失败")?;
        Ok(())
    }
}

/// 拆解 tracker：插件的无状态单轮轮询入口。
#[derive(Clone)]
pub struct PlanDecomposeTracker {
    service: PlanDecomposeService,
}

impl PlanDecomposeTracker {
    pub fn new(service: PlanDecomposeService) -> Self {
        Self { service }
    }
}

impl ru41::ingress::schedule::SimplePollJob for PlanDecomposeTracker {
    type Report = DecomposeReport;

    fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.service.config.interval_secs)
    }

    async fn poll(&self) -> anyhow::Result<Self::Report> {
        // Vault 的 git 操作是阻塞的，放到阻塞线程池里，别占插件的 tokio 线程。
        let service = self.service.clone();
        tokio::task::spawn_blocking(move || service.poll_once())
            .await
            .map_err(|e| anyhow::anyhow!("拆解轮询线程异常: {e}"))?
    }
}

enum Advance {
    Applied,
    Waiting,
    Failed(String),
}

/// 内容哈希（FNV-1a 64）：同一份内容同一个版本标识。
fn sha_like(content: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// 把「拆解结果」直接应用到文档（供测试与手工调用）。
#[cfg(test)]
pub fn apply_to_document(
    content: &str,
    summary: &str,
) -> Result<Vec<(String, Vec<ru41::domain::plan::PlanStep>)>> {
    let plan = plan::parse_plan(content).map_err(|e| anyhow::anyhow!("解析规划文档失败: {e}"))?;
    let expected = plan.needs_llm_steps();
    let steps = plan::extract_steps(summary, &expected).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"---
id = "rust"
name = "学 Rust 基础"
kind = "daily"
weight = 50.0
---

# 目标

能读懂并写出安全的 Rust 代码。

## 验收标准

- 能从零写出一个带测试的 CLI 工具

## M1 环境与基本语法

能 cargo new 一个项目。

## M2 所有权与借用

借用规则。
"#;

    #[test]
    fn sha_like_is_stable_and_sensitive() {
        assert_eq!(sha_like("abc"), sha_like("abc"));
        assert_ne!(sha_like("abc"), sha_like("abd"));
    }

    #[test]
    fn request_key_includes_doc_and_version() {
        let key = decompose_request_key("a.md", "deadbeef");
        assert!(key.starts_with("plan:decompose:a.md:"));
    }

    #[test]
    fn apply_to_document_round_trip() {
        let summary = r#"```ru41-plan-decompose
{"milestones":[
 {"name":"M1 环境与基本语法","steps":[{"name":"读 Book Ch1","minutes":60}]},
 {"name":"M2 所有权与借用","steps":[{"name":"读 Book Ch4","minutes":120}]}
]}
```"#;
        let steps = apply_to_document(DOC, summary).expect("应能应用");
        assert_eq!(steps.len(), 2);
        let updated = plan::apply_steps(DOC, &steps).expect("应能写回");
        assert!(updated.contains("- [ ] 读 Book Ch1（60）"));
        assert!(updated.contains("- [ ] 读 Book Ch4（120）"));
        // 再解析一次，应该不再有「待拆解」
        let reparsed = plan::parse_plan(&updated).expect("应能再解析");
        assert!(reparsed.needs_llm_steps().is_empty());
    }

    #[test]
    fn apply_to_document_rejects_bad_output() {
        let summary = r#"```ru41-plan-decompose
{"milestones":[{"name":"M1 环境与基本语法","steps":[{"name":"x","minutes":60}]}]}
```"#;
        assert!(apply_to_document(DOC, summary).is_err(), "漏拆 M2 应被拒");
    }
}
