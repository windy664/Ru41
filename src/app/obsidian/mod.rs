//! Obsidian 闭环服务：git 同步与冲突合并（sync）、收件箱整理（organize）、
//! 待办视图双向同步（todo）。公共 API 一律从本模块 re-export，外部引用路径不变。

mod organize;
mod sync;
mod todo;

pub use organize::{
    ORGANIZE_INDEX_FILE, OrganizeMove, OrganizePlan, OrganizeTodo, append_quick_note, extract_plan,
    organize_prompt,
};
pub use sync::PENDING_BRANCH;

use crate::{
    domain::NotifyRoute,
    ports::{channel::NotifyChannel, storage::TaskStore, vault::Vault},
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

/// Obsidian 闭环运行时配置（由 ObsidianSection::resolved 产出）。
#[derive(Debug, Clone)]
pub struct ObsidianConfig {
    pub vault_path: PathBuf,
    pub remote: String,
    pub branch: String,
    /// 只自动整理这个目录前缀下的变更；其余提交只推进游标。
    pub inbox_dir: String,
    pub interval_secs: u64,
    pub backend: String,
    /// 整理完成通知路由；None 时只整理不推送。
    pub notify: Option<NotifyRoute>,
    /// rebase 冲突 AI 合并设置；None = 关闭或无 key（冲突降级为通知人工）。
    pub merge: Option<crate::app::merge::MergeSettings>,
}

/// 由 [obsidian] 配置段解析运行时配置。ObsidianSection 数据类型住在
/// ru41-plugin-api（配置属于插件边界），产出物 ObsidianConfig 是宿主
/// 运行时概念，解析逻辑以扩展 trait 留在宿主。
pub trait ObsidianSectionExt {
    /// 解析运行时配置。不带供应商信息——AI 合并因此不启用
    /// （只有需要合并的调用方才用 [`ObsidianSectionExt::resolved_with_provider`]）。
    fn resolved(&self, notify: Option<NotifyRoute>) -> Result<ObsidianConfig>;

    /// 同上，但带上底座供应商（[agent.pi]）：AI 合并复用同一份端点/模型/协议/密钥。
    fn resolved_with_provider(
        &self,
        notify: Option<NotifyRoute>,
        agent: &crate::bootstrap::config::AgentSection,
    ) -> Result<ObsidianConfig>;
}

impl ObsidianSectionExt for crate::bootstrap::config::ObsidianSection {
    fn resolved(&self, notify: Option<NotifyRoute>) -> Result<ObsidianConfig> {
        self.resolved_with_provider(notify, &Default::default())
    }

    fn resolved_with_provider(
        &self,
        notify: Option<NotifyRoute>,
        agent: &crate::bootstrap::config::AgentSection,
    ) -> Result<ObsidianConfig> {
        use crate::bootstrap::config::ObsidianSection as S;
        let vault_path = self
            .vault_path
            .clone()
            .filter(|p| !p.trim().is_empty())
            .context("obsidian.vault_path required")?;
        Ok(ObsidianConfig {
            vault_path: PathBuf::from(vault_path),
            remote: self
                .remote
                .clone()
                .unwrap_or_else(|| S::DEFAULT_REMOTE.into()),
            branch: self
                .branch
                .clone()
                .unwrap_or_else(|| S::DEFAULT_BRANCH.into()),
            inbox_dir: self
                .inbox_dir
                .clone()
                .unwrap_or_else(|| S::DEFAULT_INBOX_DIR.into()),
            interval_secs: self.interval_secs.unwrap_or(S::DEFAULT_INTERVAL_SECS),
            backend: self
                .backend
                .clone()
                .unwrap_or_else(|| crate::bootstrap::config::AgentSection::DEFAULT_BACKEND.into()),
            notify,
            merge: resolve_merge(self, agent),
        })
    }
}

/// AI 合并设置：默认直接复用底座供应商（[agent.pi]）的端点、模型、协议与密钥
/// 变量——供应商只在一处定义，obsidian.merge_* 只是覆盖项。
/// 显式 merge_ai=false 关闭；key 缺失不阻断启动（冲突时降级为通知人工）。
fn resolve_merge(
    section: &crate::bootstrap::config::ObsidianSection,
    agent: &crate::bootstrap::config::AgentSection,
) -> Option<crate::app::merge::MergeSettings> {
    if section.merge_ai == Some(false) {
        return None;
    }
    let pi = &agent.pi;
    let env = section
        .merge_api_key_env
        .clone()
        .or_else(|| pi.api_key_env.clone())
        .unwrap_or_else(|| "MINIMAX_API_KEY".into());
    let api_key = std::env::var(&env).ok().filter(|v| !v.trim().is_empty())?;
    let base_url = section
        .merge_base_url
        .clone()
        .or_else(|| pi.base_url.clone())
        .filter(|u| !u.trim().is_empty())?;
    let model = section
        .merge_model
        .clone()
        .or_else(|| pi.model.clone())
        .filter(|m| !m.trim().is_empty())?;
    let api = section
        .merge_api
        .as_deref()
        .or(pi.api.as_deref())
        .map(crate::app::merge::MergeApi::parse)
        .unwrap_or(crate::app::merge::MergeApi::AnthropicMessages);
    Some(crate::app::merge::MergeSettings {
        base_url,
        model,
        api_key,
        api,
    })
}

/// 一轮轮询的结果报告。
#[derive(Debug, Serialize)]
pub struct ObsidianPollReport {
    /// idle / waiting / enqueued / committed / conflict / retry / self-commit / skipped
    pub action: &'static str,
    pub remote_head: String,
    pub changed_files: usize,
    pub committed: bool,
    pub notified: bool,
    /// 本轮自动合并掉的同步冲突副本（原文件路径）。
    #[serde(default)]
    pub merged_conflicts: Vec<String>,
    /// 本轮从远端待合并分支并入 master 的提交数。
    #[serde(default)]
    pub merged_remote_commits: u32,
}

/// 处理状态：records 文档 obsidian_state:main。
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ObsidianState {
    /// 已同步/索引到的远端提交；空表示尚未建立基线，不作为整理触发器。
    #[serde(default)]
    pub(crate) processed_sha: String,
    /// 正在由 Agent 整理的提交。
    #[serde(default)]
    pub(crate) pending: Option<PendingOrganize>,
    /// 最近一次完成每周整理的时间；0 表示尚未整理，首轮立即执行。
    #[serde(default)]
    pub(crate) last_organized_at: i64,
    /// 最近一次失败/冲突已通知的目标提交（同一提交不重复通知）。
    #[serde(default)]
    pub(crate) failed_sha: Option<String>,
    /// 同一提交的自动重试次数；达到上限后放弃等人工处理。
    #[serde(default)]
    pub(crate) failed_count: u32,
    /// 已放弃的提交：不再自动入队，人工删除状态文档后重置。
    #[serde(default)]
    pub(crate) abandoned: Option<String>,
    /// 上次知识库重建的 Unix 秒；0 表示从未重建。
    #[serde(default)]
    pub(crate) last_reindex_at: i64,
    /// 有未索引的内容变更（冷却期跳过时置位，冷却结束后补建）。
    #[serde(default)]
    pub(crate) index_dirty: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PendingOrganize {
    pub(crate) task_id: i64,
    pub(crate) target_sha: String,
    pub(crate) files: Vec<String>,
}

/// Obsidian 闭环追踪器：git 轮询与游标是确定性 Rust，归档判断交给 Agent 任务。
/// Obsidian Vault 核心服务：Git 轮询与游标是确定性代码，归档判断交给 Agent 任务。
/// 属于核心能力（app 层）：digest 落 Vault、周报、博客管线等附属都经它读写。
#[derive(Clone)]
pub struct ObsidianService {
    pub(crate) store: Arc<dyn TaskStore>,
    pub(crate) vault: Arc<dyn Vault>,
    pub(crate) notifier: Option<Arc<dyn NotifyChannel>>,
    pub(crate) notify_target: String,
    pub(crate) config: ObsidianConfig,
    /// 可选的知识库索引器：vault 内容变更后后台重建向量索引（RAG）。
    knowledge: Option<Arc<crate::app::knowledge::KnowledgeService>>,
    /// 可选的待办视图投影：todo 数据库 → `07-Todo-待办/当前待办.md`（机器文件）。
    pub(crate) todo_options: Option<crate::domain::todo::TodoOptions>,
}

impl ObsidianService {
    pub fn new(
        store: Arc<dyn TaskStore>,
        vault: Arc<dyn Vault>,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: ObsidianConfig,
    ) -> Self {
        let notify_target = config
            .notify
            .as_ref()
            .map(|route| route.target.clone())
            .unwrap_or_default();
        Self {
            store,
            vault,
            notifier,
            notify_target,
            config,
            knowledge: None,
            todo_options: None,
        }
    }

    /// 挂载知识库索引器；不挂则同步只管 git 闭环，不建向量索引。
    pub fn with_knowledge(
        mut self,
        knowledge: Arc<crate::app::knowledge::KnowledgeService>,
    ) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    /// 挂载待办视图投影：每轮把 todo 数据库概况渲染进
    /// `07-Todo-待办/当前待办.md`（内容没变不写；变更自提交）。
    pub fn with_todo_view(mut self, options: crate::domain::todo::TodoOptions) -> Self {
        self.todo_options = Some(options);
        self
    }

    pub fn vault(&self) -> &Arc<dyn Vault> {
        &self.vault
    }

    pub(crate) fn load_state(&self) -> Result<ObsidianState> {
        match self.store.document("obsidian_state", "main")? {
            Some(doc) => Ok(serde_json::from_value(doc.value).context("invalid obsidian_state")?),
            None => Ok(ObsidianState::default()),
        }
    }

    pub(crate) fn save_state(&self, state: &ObsidianState) -> Result<()> {
        // put_document(None) 是 insert-or-ignore，不会覆盖；状态文档必须按 revision 乐观更新。
        let revision = self
            .store
            .document("obsidian_state", "main")?
            .map(|d| d.revision);
        let written = self.store.put_document(
            "obsidian_state",
            "main",
            revision,
            &serde_json::to_value(state)?,
        )?;
        anyhow::ensure!(written, "obsidian_state 被并发修改，本轮状态未保存");
        Ok(())
    }

    pub async fn poll_once(&self) -> Result<ObsidianPollReport> {
        let vault = self.vault.clone();
        let target = tokio::task::spawn_blocking(move || -> Result<String> {
            vault.fetch()?;
            vault.remote_head()
        })
        .await??;
        let mut state = self.load_state()?;
        let head_before = state.processed_sha.clone();
        // 本地与远端对齐：todo 视图等自提交可能因远端先动而推失败，
        // 每轮 rebase+push 兜底，分歧不再靠人肉修。整理任务进行中跳过
        // （工作区有 agent 未提交的搬运，rebase 会拒）。
        if state.pending.is_none() {
            let vault = self.vault.clone();
            match tokio::task::spawn_blocking(move || vault.sync()).await? {
                Ok(crate::ports::vault::SyncOutcome::Clean)
                | Ok(crate::ports::vault::SyncOutcome::Pushed) => {}
                Ok(crate::ports::vault::SyncOutcome::Conflict(files)) => {
                    // 冲突现场还在：AI 语义合并；失败一律 abort 还原 + 通知人工。
                    if let Err(error) = self.merge_conflicts(&files).await {
                        eprintln!("[obsidian] vault conflict ai-merge failed: {error:#}");
                        let vault = self.vault.clone();
                        let _ = tokio::task::spawn_blocking(move || vault.rebase_abort()).await;
                        self.notify_merge_failed(&files, &error).await?;
                    }
                }
                Err(error) => eprintln!("[obsidian] vault sync failed: {error:#}"),
            }
        }
        // 同步冲突副本：文件级同步（Syncthing 等）撞车时会把其中一份另存为
        // `名字.sync-conflict-<时间戳>-<设备>.md`。这里自动把两份合并回原文件、
        // 删掉副本——不必人工去认这些怪文件。
        let merged_conflicts = self.resolve_sync_conflicts().await?;
        // 远端合并队列：本地电脑推来的待合并分支，由服务器并进 master。
        let merged_remote_commits = self.merge_remote_pending().await?;
        let mut report = ObsidianPollReport {
            action: "idle",
            remote_head: target.clone(),
            changed_files: 0,
            committed: false,
            notified: false,
            merged_conflicts,
            merged_remote_commits,
        };

        if let Some(pending) = state.pending.clone() {
            let (action, notified) = self.handle_pending(&mut state, &pending, &target).await?;
            report.action = action;
            report.notified = notified;
            report.committed = action == "committed";
            report.changed_files = pending.files.len();
        } else {
            if target != state.processed_sha {
                report.action = self.handle_new_head(&mut state, &target).await?;
            }
            // Git 同步仍每轮进行，AI 整理只按七天周期启动；到点时扫描
            // 当前 Inbox 全量文件，不能依赖新提交 diff，否则周期内写入会漏掉。
            if self.organize_due(&state) {
                report.action = self.start_organize(&mut state, &target).await?;
                report.changed_files = state.pending.as_ref().map(|p| p.files.len()).unwrap_or(0);
            }
        }
        self.maybe_reindex(&mut state, &head_before, &target, report.committed);
        self.save_state(&state)?;
        if let Err(error) = self.render_todo_view().await {
            // 视图渲染失败不影响 git 闭环本体，下轮自然重试。
            eprintln!("[obsidian] todo view render failed: {error:#}");
        }
        Ok(report)
    }

    /// vault 内容变更后后台重建知识库索引（衍生数据，全量重灌）。
    /// 整理任务进行中跳过：agent 正在搬文件，等整理提交后的下一轮再索引。
    /// 冷却：chaoxing/zhihuishu 等兄弟模块每几分钟就往同一仓库推自提交，
    /// 全量重建是分钟级满载 embedding，不节流会把小机器烧到热保护——
    /// 变更先置 dirty，冷却窗结束才真正重建。
    fn maybe_reindex(
        &self,
        state: &mut ObsidianState,
        head_before: &str,
        target: &str,
        committed: bool,
    ) {
        let Some(knowledge) = &self.knowledge else {
            return;
        };
        /// 两次全量重建的最小间隔（秒）。
        const REINDEX_COOLDOWN_SECS: i64 = 1800;
        if committed || head_before != target {
            state.index_dirty = true;
        }
        if !state.index_dirty || state.pending.is_some() {
            return;
        }
        let now = crate::util::now();
        if now - state.last_reindex_at < REINDEX_COOLDOWN_SECS {
            return;
        }
        // 乐观记账：失败也会由下一轮的新变更重新置 dirty。
        state.last_reindex_at = now;
        state.index_dirty = false;
        let knowledge = knowledge.clone();
        let vault_path = self.config.vault_path.clone();
        tokio::task::spawn_blocking(move || match knowledge.index_vault(&vault_path) {
            Ok(report) => eprintln!(
                "[knowledge] reindexed: {} files, {} chunks, {}ms",
                report.files, report.chunks, report.elapsed_ms
            ),
            Err(error) => eprintln!("[knowledge] reindex failed: {error:#}"),
        });
    }
}

impl crate::ingress::schedule::PollJob for ObsidianService {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value>> + Send + '_>>
    {
        // 经 plugin-api 桥到插件/宿主自己的 runtime：本服务会被插件 .so 静态链接，
        // 插件 tokio 的 reactor 与宿主不通用（blog 插件 2026-10-27 事故同款）。
        let owned = self.clone();
        ru41_plugin_api::bridge::bridge(async move {
            Ok(serde_json::to_value(owned.poll_once().await?)?)
        })
    }
}
