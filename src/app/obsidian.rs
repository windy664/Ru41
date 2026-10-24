use crate::{
    adapters::vault::EMPTY_TREE,
    app::task::TaskService,
    domain::NotifyRoute,
    ports::{channel::NotifyChannel, storage::TaskStore, vault::Vault},
};
use anyhow::{Context, Result, bail};
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
}

/// 处理状态：records 文档 obsidian_state:main。
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct ObsidianState {
    /// 已处理（含跳过）到的远端提交；空表示尚未建立基线。
    #[serde(default)]
    processed_sha: String,
    /// 正在由 Agent 整理的提交。
    #[serde(default)]
    pending: Option<PendingOrganize>,
    /// 最近一次失败/冲突已通知的目标提交（同一提交不重复通知）。
    #[serde(default)]
    failed_sha: Option<String>,
    /// 同一提交的自动重试次数；达到上限后放弃等人工处理。
    #[serde(default)]
    failed_count: u32,
    /// 已放弃的提交：不再自动入队，人工删除状态文档后重置。
    #[serde(default)]
    abandoned: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PendingOrganize {
    task_id: i64,
    target_sha: String,
    files: Vec<String>,
}

/// Obsidian 闭环追踪器：git 轮询与游标是确定性 Rust，归档判断交给 Agent 任务。
/// Obsidian Vault 核心服务：Git 轮询与游标是确定性代码，归档判断交给 Agent 任务。
/// 属于核心能力（app 层）：digest 落 Vault、周报、博客管线等附属都经它读写。
pub struct ObsidianService {
    store: Arc<dyn TaskStore>,
    vault: Arc<dyn Vault>,
    notifier: Option<Arc<dyn NotifyChannel>>,
    notify_target: String,
    config: ObsidianConfig,
    /// 可选的知识库索引器：vault 内容变更后后台重建向量索引（RAG）。
    knowledge: Option<Arc<crate::app::knowledge::KnowledgeService>>,
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

    pub fn vault(&self) -> &Arc<dyn Vault> {
        &self.vault
    }

    fn load_state(&self) -> Result<ObsidianState> {
        match self.store.document("obsidian_state", "main")? {
            Some(doc) => Ok(serde_json::from_value(doc.value).context("invalid obsidian_state")?),
            None => Ok(ObsidianState::default()),
        }
    }

    fn save_state(&self, state: &ObsidianState) -> Result<()> {
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
        let mut report = ObsidianPollReport {
            action: "idle",
            remote_head: target.clone(),
            changed_files: 0,
            committed: false,
            notified: false,
        };

        if let Some(pending) = state.pending.clone() {
            let (action, notified) = self.handle_pending(&mut state, &pending, &target).await?;
            report.action = action;
            report.notified = notified;
            report.committed = action == "committed";
            report.changed_files = pending.files.len();
        } else if target != state.processed_sha {
            report.action = self.handle_new_head(&mut state, &target).await?;
            report.changed_files = state.pending.as_ref().map(|p| p.files.len()).unwrap_or(0);
        }
        self.save_state(&state)?;
        self.maybe_reindex(&state, &head_before, &target, report.committed);
        Ok(report)
    }

    /// vault 内容变更后后台重建知识库索引（衍生数据，全量重灌）。
    /// 整理任务进行中跳过：agent 正在搬文件，等整理提交后的下一轮再索引。
    fn maybe_reindex(
        &self,
        state: &ObsidianState,
        head_before: &str,
        target: &str,
        committed: bool,
    ) {
        let Some(knowledge) = &self.knowledge else {
            return;
        };
        let changed = committed || head_before != target;
        if !changed || state.pending.is_some() {
            return;
        }
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

    /// 有进行中的整理任务：按任务状态推进（成功→提交推送，失败→下轮重试）。
    /// 返回 (动作, 是否已发送整理通知)。
    async fn handle_pending(
        &self,
        state: &mut ObsidianState,
        pending: &PendingOrganize,
        target: &str,
    ) -> Result<(&'static str, bool)> {
        let Some(task) = self.store.get(pending.task_id)? else {
            state.pending = None;
            return Ok(("retry", false));
        };
        use crate::domain::TaskStatus;
        match task.status {
            TaskStatus::Succeeded => {
                // 提交前再确认远端没动；动了说明人工又推了提交，放弃本次整理下轮重做。
                let vault = self.vault.clone();
                let expected = pending.target_sha.clone();
                let outcome = tokio::task::spawn_blocking(move || -> Result<String> {
                    vault.fetch()?;
                    let head = vault.remote_head()?;
                    if head != expected {
                        vault.checkout_hard(&head)?;
                        return Ok("conflict".into());
                    }
                    let committed = vault.commit_all(&format!(
                        "[ru41] organize inbox {}",
                        &expected[..expected.len().min(7)]
                    ))?;
                    if committed {
                        vault.push()?;
                        Ok("committed".into())
                    } else {
                        Ok("clean".into())
                    }
                })
                .await??;
                match outcome.as_str() {
                    "conflict" => {
                        state.pending = None;
                        let should_notify = self
                            .notify_once(state, &pending.target_sha, "conflict")
                            .await?;
                        if should_notify {
                            self.notify_conflict(pending).await?;
                        }
                        Ok(("conflict", should_notify))
                    }
                    _ => {
                        state.processed_sha = pending.target_sha.clone();
                        state.pending = None;
                        state.failed_sha = None;
                        state.failed_count = 0;
                        if outcome == "committed" && target == pending.target_sha {
                            let summary = task.result.map(|r| r.summary).unwrap_or_default();
                            self.notify_organized(pending.files.len(), &summary).await?;
                        }
                        Ok((
                            if outcome == "committed" {
                                "committed"
                            } else {
                                "clean"
                            },
                            outcome == "committed" && target == pending.target_sha,
                        ))
                    }
                }
            }
            TaskStatus::Failed | TaskStatus::Cancelled => {
                // 自动重试（恢复三次尝试预算由 worker 重新领取）；同一提交只通知一次，
                // 连续失败超过上限则放弃，等新提交或人工清理状态文档。
                const MAX_RETRIES: u32 = 3;
                let should_notify = self
                    .notify_once(state, &pending.target_sha, "failed")
                    .await?;
                if should_notify {
                    self.notify_failed_once(&pending.target_sha, task.last_error.as_deref())
                        .await?;
                }
                state.failed_count += 1;
                if state.failed_count >= MAX_RETRIES {
                    state.pending = None;
                    state.abandoned = Some(pending.target_sha.clone());
                    self.notify_abandoned(pending).await?;
                    return Ok(("abandoned", true));
                }
                if TaskService::new(self.store.clone())
                    .retry(pending.task_id)
                    .is_err()
                {
                    state.pending = None;
                }
                Ok(("retry", should_notify))
            }
            _ => Ok(("waiting", false)),
        }
    }

    /// 同一目标提交只通知一次的开关；返回本次是否应发通知。
    async fn notify_once(
        &self,
        state: &mut ObsidianState,
        sha: &str,
        _reason: &str,
    ) -> Result<bool> {
        if state.failed_sha.as_deref() == Some(sha) {
            return Ok(false);
        }
        state.failed_sha = Some(sha.to_owned());
        Ok(self.notifier.is_some() && !self.notify_target.is_empty())
    }

    /// 发现新提交：自身提交直接推进游标；Inbox 变更入队整理任务；其余只推进游标。
    async fn handle_new_head(
        &self,
        state: &mut ObsidianState,
        target: &str,
    ) -> Result<&'static str> {
        if state.abandoned.as_deref() == Some(target) {
            return Ok("abandoned");
        }
        let vault = self.vault.clone();
        let sha = target.to_owned();
        let subject = tokio::task::spawn_blocking(move || vault.head_subject(&sha)).await??;
        if subject.starts_with("[ru41]") {
            state.processed_sha = target.to_owned();
            return Ok("self-commit");
        }
        let vault = self.vault.clone();
        let sha = target.to_owned();
        let from = state.processed_sha.clone();
        let files = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let base = if from.is_empty() {
                EMPTY_TREE.to_owned()
            } else {
                from
            };
            vault.changed_files(&base, &sha)
        })
        .await??;
        let inbox_prefix = format!("{}/", self.config.inbox_dir);
        let files: Vec<String> = files
            .into_iter()
            .filter(|f| f.starts_with(&inbox_prefix))
            .collect();
        if files.is_empty() {
            state.processed_sha = target.to_owned();
            return Ok("skipped");
        }
        // 工作区对齐到目标提交后再交给 Agent，避免在旧基线上整理。
        let vault = self.vault.clone();
        let sha = target.to_owned();
        tokio::task::spawn_blocking(move || vault.checkout_hard(&sha)).await??;
        let prompt = organize_prompt(&self.config.vault_path, &self.config.inbox_dir, &files);
        let result = TaskService::new(self.store.clone()).enqueue_for(
            &format!("obsidian:organize:{target}"),
            &prompt,
            &self.config.backend,
        )?;
        state.pending = Some(PendingOrganize {
            task_id: result.task.id,
            target_sha: target.to_owned(),
            files,
        });
        // 新入队意味着新尝试：清掉旧失败记录。
        state.failed_sha = None;
        state.failed_count = 0;
        state.abandoned = None;
        Ok("enqueued")
    }

    /// 整理成功的通知：附带 Agent 的输出摘要，QQ 里直接可审每个文件的处理结果。
    async fn notify_organized(&self, file_count: usize, summary: &str) -> Result<bool> {
        let Some(notifier) = &self.notifier else {
            return Ok(false);
        };
        let summary = crate::util::truncate_chars(summary.trim(), 1500);
        let text = if summary.is_empty() {
            format!(
                "## Obsidian 已整理\n\nInbox 的 {file_count} 个变更已归档并推送，本地同步后可在 Obsidian 查看。"
            )
        } else {
            format!("## Obsidian 已整理\n\nInbox 的 {file_count} 个变更已归档并推送：\n\n{summary}")
        };
        notifier.send(&self.notify_target, None, &text).await?;
        Ok(true)
    }

    /// 失败通知：同一提交只发一次，附最近一次错误，自动重试中。
    async fn notify_failed_once(&self, sha: &str, error: Option<&str>) -> Result<()> {
        let Some(notifier) = &self.notifier else {
            return Ok(());
        };
        let error = error
            .map(|e| format!("\n\n> {}", crate::util::truncate_chars(e, 300)))
            .unwrap_or_default();
        notifier
            .send(
                &self.notify_target,
                None,
                &format!(
                    "## Obsidian 整理失败\n\n提交 `{sha}` 的整理任务失败，已自动重试。{error}"
                ),
            )
            .await?;
        Ok(())
    }

    /// 冲突通知：任务成功后远端又被推进，本次整理被放弃。
    async fn notify_conflict(&self, pending: &PendingOrganize) -> Result<()> {
        let Some(notifier) = &self.notifier else {
            return Ok(());
        };
        notifier
            .send(
                &self.notify_target,
                None,
                &format!(
                    "## Obsidian 整理冲突\n\n提交 `{}` 整理完成后，远端又有新提交，本次整理已放弃，下一轮会基于新提交重新整理。",
                    pending.target_sha
                ),
            )
            .await?;
        Ok(())
    }

    /// 放弃通知：同一提交连续失败超过上限，等人处理。
    async fn notify_abandoned(&self, pending: &PendingOrganize) -> Result<()> {
        let Some(notifier) = &self.notifier else {
            return Ok(());
        };
        notifier
            .send(
                &self.notify_target,
                None,
                &format!(
                    "## Obsidian 整理已放弃\n\n提交 `{}` 的整理任务连续失败，已停止自动重试。\n\n请检查任务详情后重试，或删除 `obsidian_state` 状态文档重置。",
                    pending.target_sha
                ),
            )
            .await?;
        Ok(())
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
        Box::pin(async move { Ok(serde_json::to_value(self.poll_once().await?)?) })
    }
}

/// 整理任务 prompt：只描述事实与边界，归档规则由 obsidian-organize skill 提供。
fn organize_prompt(vault_path: &std::path::Path, inbox_dir: &str, files: &[String]) -> String {
    let list = files
        .iter()
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "整理 Obsidian Vault 的收件箱。Vault 根目录：{}。\n\
         本次同步带来以下 {inbox_dir} 变更：\n{list}\n\n\
         按 obsidian-organize 技能归档：阅读每条内容，移动到合适的目录并规范标题，\
         修复受影响的双链，保留原始内容不删减。\n\
         只读写 Vault 目录内的文件，不要执行任何 git 命令（提交推送由程序负责），\
         完成后输出每个文件的处理结果摘要。",
        vault_path.display()
    )
}

/// QQ 快速记录：追加 Inbox 笔记并立即提交推送。
pub fn append_quick_note(
    vault: &dyn Vault,
    inbox_dir: &str,
    text: &str,
    now: i64,
    offset_seconds: i32,
) -> Result<String> {
    let relative = vault.append_inbox_note(inbox_dir, text, now, offset_seconds)?;
    if !vault.commit_all("[ru41] quick note")? {
        bail!("quick note produced no commit");
    }
    vault.push()?;
    Ok(relative)
}
