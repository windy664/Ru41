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
    /// 可选的待办视图投影：todo 数据库 → `07-Todo-待办/当前待办.md`（机器文件）。
    todo_options: Option<crate::domain::todo::TodoOptions>,
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
        // 本地与远端对齐：todo 视图等自提交可能因远端先动而推失败，
        // 每轮 rebase+push 兜底，分歧不再靠人肉修。整理任务进行中跳过
        // （工作区有 agent 未提交的搬运，rebase 会拒）。
        if state.pending.is_none() {
            let vault = self.vault.clone();
            match tokio::task::spawn_blocking(move || vault.sync()).await? {
                Ok(_) => {}
                Err(error) => eprintln!("[obsidian] vault sync failed: {error:#}"),
            }
        }
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
        if let Err(error) = self.render_todo_view().await {
            // 视图渲染失败不影响 git 闭环本体，下轮自然重试。
            eprintln!("[obsidian] todo view render failed: {error:#}");
        }
        Ok(report)
    }

    /// 待办视图：todo 数据库 ↔ `04-Life-生活/Todo-待办.md` 双向同步。
    /// 数据库 → 文件：每轮重渲（内容没变不写）；文件 → 数据库：先读旧文件里的
    /// 勾选状态写回（打勾=完成、取消勾=恢复），再按最新状态渲染。
    /// 变更以 [ru41] 前缀自提交（下一轮游标自认，不会触发整理）。
    async fn render_todo_view(&self) -> Result<()> {
        let Some(options) = self.todo_options else {
            return Ok(());
        };
        let service = crate::app::todo::TodoService::new(self.store.clone(), options);
        let now = crate::util::now();
        let vault = self.vault.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            if let Some(existing) = vault.read_file(TODO_VIEW_PATH)? {
                // 1. 勾选回写：旧文件里用户手动的 [x]/[ ] 变更写回数据库。
                for (id, checked) in parse_checkbox_states(&existing) {
                    let result = if checked {
                        service.complete(&id, now).map(|_| ())
                    } else {
                        service.restore(&id, now).map(|_| ())
                    };
                    if let Err(error) = result {
                        // 状态已一致或 id 已失效：忽略单项，不挡整轮。
                        eprintln!("[obsidian] todo checkbox sync {id}: {error:#}");
                    }
                }
                // 2. 新增回写：无 `#id` 锚点的 `- [ ]` 行是 Obsidian 里手写的新待办。
                let known_titles: std::collections::HashSet<String> = service
                    .list(crate::domain::todo::TodoView::All, now, 1000)?
                    .into_iter()
                    .filter(|item| item.status == crate::domain::todo::TodoStatus::Active)
                    .map(|item| item.title)
                    .collect();
                for (title, due_text) in parse_new_task_lines(&existing) {
                    if known_titles.contains(&title) {
                        continue; // 同名活跃待办已存在，不重复建
                    }
                    let due_at = due_text.as_deref().and_then(|text| {
                        crate::domain::todo::parse_due(text, now, &options).ok()
                    });
                    let input = crate::domain::todo::TodoInput {
                        title: title.clone(),
                        priority: crate::domain::todo::TodoPriority::Normal,
                        labels: vec![],
                        due_at,
                        remind_at: due_at,
                        source: Some(crate::domain::todo::TodoSource {
                            channel: "obsidian".into(),
                            source_id: None,
                        }),
                    };
                    if let Err(error) = service.add(input, now) {
                        eprintln!("[obsidian] todo add from view {title:?}: {error:#}");
                    }
                }
            }
            // 3. 按最新数据库状态渲染。
            let overview = service.overview(now, 50)?;
            let completed_recent: Vec<crate::domain::todo::TodoItem> = service
                .list(crate::domain::todo::TodoView::All, now, 1000)?
                .into_iter()
                .filter(|item| {
                    item.status == crate::domain::todo::TodoStatus::Completed
                        && item
                            .completed_at
                            .is_some_and(|at| at >= now - 7 * 86_400)
                })
                .collect();
            let content = render_todo_markdown(&overview, &completed_recent, now, options);
            if vault.read_file(TODO_VIEW_PATH)?.as_deref() == Some(content.as_str()) {
                return Ok(());
            }
            vault.write_file(TODO_VIEW_PATH, &content)?;
            if vault.commit_all("[ru41] todo 视图同步")? {
                vault.push()?;
            }
            Ok(())
        })
        .await??;
        Ok(())
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
/// 待办视图在 vault 里的位置（与数据库双向同步：勾选状态会写回）。
const TODO_VIEW_PATH: &str = "04-Life-生活/Todo-待办.md";

/// 从视图文件解析勾选状态：`- [ ]`/`- [x]` 行尾的 `` `#id` `` → (id, 是否勾选)。
fn parse_checkbox_states(content: &str) -> Vec<(String, bool)> {
    let mut states = Vec::new();
    for line in content.lines() {
        let line = line.trim_start();
        // "- [ ]" 剥掉 "- [" 后剩 " ] ..."（前导空格）；"- [x]" 剩 "x] ..."。
        let checked = match line.strip_prefix("- [") {
            Some(rest) if rest.starts_with(" ] ") => false,
            Some(rest) if rest.starts_with("x] ") || rest.starts_with("X] ") => true,
            _ => continue,
        };
        // 行尾 `` `#id` `` 标记
        let Some(start) = line.rfind("`#") else {
            continue;
        };
        let id = &line[start + 2..];
        let Some(end) = id.find('`') else {
            continue;
        };
        let id = id[..end].trim();
        if !id.is_empty() {
            states.push((id.to_owned(), checked));
        }
    }
    states
}

/// 解析视图里用户手写的新待办行：`- [ ] <标题>` 且无 `` `#id` `` 锚点。
/// 支持 Obsidian Tasks 的 `📅 YYYY-MM-DD` 截止标记（从标题剔除，单独返回）。
fn parse_new_task_lines(content: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim_start();
        let Some(text) = line.strip_prefix("- [ ] ") else {
            continue;
        };
        if text.contains("`#") {
            continue; // 有锚点的是数据库渲染行，不是新增
        }
        let (title, due) = match text.find("📅 ") {
            Some(pos) => {
                let due = text[pos + "📅 ".len()..]
                    .split_whitespace()
                    .next()
                    .map(str::to_owned);
                (text[..pos].trim().to_owned(), due)
            }
            None => (text.trim().to_owned(), None),
        };
        if !title.is_empty() {
            out.push((title, due));
        }
    }
    out
}


/// 分节：已过期/今天/近期/长期/未排期 + 近 7 天已完成。
/// 待办概况渲染成 Obsidian 任务清单：严格 `- [ ]` 复选格式，行尾 `` `#id` `` 供回写。
/// 分节：已过期/今天/近期/长期/未排期 + 近 7 天已完成。
fn render_todo_markdown(
    overview: &crate::domain::todo::TodoOverview,
    completed_recent: &[crate::domain::todo::TodoItem],
    now: i64,
    options: crate::domain::todo::TodoOptions,
) -> String {
    use crate::domain::todo::due_label;
    let mut out = String::from(
        "# 待办清单\n\n\
         > Ru41 与 Obsidian 双向同步：打勾 = 完成、取消勾 = 恢复（写回数据库）。\n\
         > 新增：直接写一行 `- [ ] 标题`（支持 `📅 YYYY-MM-DD` 截止），下一轮同步自动入库并分配锚点。\n\
         > 修改/删除走 QQ `/待办` 或 CLI `ru41 todo`；带锚点的正文行会被覆盖，勾选状态保留。\n",
    );
    let sections: [(&str, &Vec<crate::domain::todo::TodoItem>, bool); 5] = [
        ("已过期", &overview.overdue, true),
        ("今天", &overview.today, false),
        ("近期", &overview.recent, false),
        ("长期", &overview.long_term, false),
        ("未排期", &overview.inbox, false),
    ];
    for (title, items, overdue) in sections {
        if items.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {title}\n"));
        for item in items {
            let due = due_label(item.due_at, now, options, overdue);
            out.push_str(&format!("- [ ] {}（{}）`#{}`\n", item.title, due, item.id));
        }
    }
    if !completed_recent.is_empty() {
        out.push_str("\n## 已完成（近 7 天）\n");
        for item in completed_recent {
            let done = item
                .completed_at
                .map(|at| crate::domain::todo::local_date(at, options.offset_seconds))
                .unwrap_or_default();
            out.push_str(&format!("- [x] {}（完成于 {}）`#{}`\n", item.title, done, item.id));
        }
    }
    if overview.active_count() == 0 && completed_recent.is_empty() {
        out.push_str("\n（空）\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_view_renders_sections_and_empty() {
        let options = crate::domain::todo::TodoOptions {
            offset_seconds: 8 * 3600,
            recent_days: 7,
            long_term_days: 30,
        };
        let empty = crate::domain::todo::TodoOverview::default();
        assert!(render_todo_markdown(&empty, &[], 0, options).contains("（空）"));

        let mut overview = crate::domain::todo::TodoOverview::default();
        overview.today.push(crate::domain::todo::TodoItem {
            id: "abc".into(),
            title: "交实验报告".into(),
            status: crate::domain::todo::TodoStatus::Active,
            priority: crate::domain::todo::TodoPriority::Normal,
            labels: vec![],
            due_at: None,
            remind_at: None,
            last_reminded_at: None,
            completed_at: None,
            created_at: 0,
            updated_at: 0,
            source: None,
        });
        let md = render_todo_markdown(&overview, &[], 0, options);
        assert!(md.contains("## 今天"));
        assert!(md.contains("- [ ] 交实验报告"));
        assert!(!md.contains("## 已过期"), "空分节不出现");
    }

    #[test]
    fn parses_new_task_lines_without_anchor() {
        let content = "- [ ] 手写的新待办\n\
             - [ ] 带截止的新待办 📅 2026-10-31\n\
             - [ ] 数据库渲染行（有锚点）`#todo_1_abc`\n\
             - [x] 已完成的不算新增\n\
             - [ ] \n";
        let new_tasks = parse_new_task_lines(content);
        assert_eq!(
            new_tasks,
            vec![
                ("手写的新待办".to_owned(), None),
                ("带截止的新待办".to_owned(), Some("2026-10-31".to_owned())),
            ]
        );
    }

    #[test]
    fn parses_checkbox_states_from_view() {
        let content = "# 待办清单\n\n## 今天\n- [ ] 交报告（今天）`#todo_1_abc`\n\
             - [x] 已做完的事（完成于 2026-10-25）`#todo_2_def`\n\
             - [X] 大写也算 `#todo_3_ghi`\n\
             - 普通列表不是任务 `#todo_4_jkl`\n\
             - [ ] 没有 id 标记的行\n";
        let states = parse_checkbox_states(content);
        assert_eq!(
            states,
            vec![
                ("todo_1_abc".to_owned(), false),
                ("todo_2_def".to_owned(), true),
                ("todo_3_ghi".to_owned(), true),
            ]
        );
    }
}

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
