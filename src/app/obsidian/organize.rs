//! 收件箱整理闭环：发现新提交 → 入队 Agent 整理任务 → 解析指令落盘 →
//! 提交推送 → 通知。Agent 只输出 ru41-organize JSON 指令，落盘由宿主经
//! Vault 执行——vault 全文与文件系统对 Agent 不可见。

use super::{ObsidianService, ObsidianState, PendingOrganize};
use crate::{adapters::vault::EMPTY_TREE, app::task::TaskService, ports::vault::Vault};
use anyhow::{Context, Result, bail};

/// 收件箱整理索引笔记（收件箱目录内，程序自动生成，agent 不得改动）。
pub const ORGANIZE_INDEX_FILE: &str = "整理索引.md";
/// 整理索引回溯的时间窗（天）。
const ORGANIZE_INDEX_DAYS: u32 = 7;

/// 由去向记录渲染索引 markdown；没有记录时也生成占位，让用户知道这页是活的。
fn organize_index_markdown(moves: &[crate::ports::vault::InboxMove], today: &str) -> String {
    let header = format!(
        "# 收件箱整理索引\n\n\
         > 程序自动生成（{today}）：近 {ORGANIZE_INDEX_DAYS} 天从收件箱归档的文件去向。\n"
    );
    if moves.is_empty() {
        return format!("{header}\n近 {ORGANIZE_INDEX_DAYS} 天没有从收件箱归档的文件。\n");
    }
    let mut rows = String::from("\n| 日期 | 文件 | 去向 |\n|---|---|---|\n");
    for m in moves {
        rows.push_str(&format!(
            "| {} | {} | {} |\n",
            m.date, m.name, m.destination
        ));
    }
    format!("{header}{rows}")
}

/// 整理提交后重建索引笔记并并入本次推送；失败只记日志，不阻断整理闭环。
fn update_organize_index(vault: &dyn Vault, inbox_dir: &str) -> Result<()> {
    let moves = vault.recent_inbox_moves(inbox_dir, ORGANIZE_INDEX_DAYS)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    vault.write_file(
        &format!("{inbox_dir}/{ORGANIZE_INDEX_FILE}"),
        &organize_index_markdown(&moves, &today),
    )?;
    vault.commit_all(&format!("[ru41] update {ORGANIZE_INDEX_FILE}"))?;
    Ok(())
}

impl ObsidianService {
    /// 有进行中的整理任务：按任务状态推进（成功→提交推送，失败→下轮重试）。
    /// 返回 (动作, 是否已发送整理通知)。
    pub(super) async fn handle_pending(
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
                // Agent 只输出指令 JSON；解析失败视为本轮失败，换 key 重入队重试
                // （retry() 只认 failed 任务，已成功任务不可重试）。
                const MAX_RETRIES: u32 = 3;
                let summary = task
                    .result
                    .as_ref()
                    .map(|r| r.summary.as_str())
                    .unwrap_or_default();
                let plan = match extract_plan(summary) {
                    Ok(plan) => plan,
                    Err(error) => {
                        eprintln!(
                            "[obsidian] organize plan invalid: {error:#}; output head: {}",
                            crate::util::truncate_chars(summary, 200)
                        );
                        let should_notify = self
                            .notify_once(state, &pending.target_sha, "failed")
                            .await?;
                        if should_notify {
                            self.notify_failed_once(
                                &pending.target_sha,
                                Some(&format!("整理指令解析失败：{error:#}")),
                            )
                            .await?;
                        }
                        state.failed_count += 1;
                        if state.failed_count >= MAX_RETRIES {
                            state.pending = None;
                            state.abandoned = Some(pending.target_sha.clone());
                            self.notify_abandoned(pending).await?;
                            return Ok(("abandoned", true));
                        }
                        let retry_key = format!("{}#r{}", "obsidian:organize", state.failed_count);
                        let key = format!("{}:{}", retry_key, pending.target_sha);
                        let prompt = task.prompt.clone();
                        let result = TaskService::new(self.store.clone()).enqueue_for(
                            &key,
                            &prompt,
                            &self.config.backend,
                        )?;
                        state.pending = Some(PendingOrganize {
                            task_id: result.task.id,
                            target_sha: pending.target_sha.clone(),
                            files: pending.files.clone(),
                        });
                        return Ok(("retry", should_notify));
                    }
                };
                // 待办先入库（同标题去重）；即使后续提交冲突，识别出的截止也不丢。
                if let Err(error) = self.apply_plan_todos(&plan) {
                    eprintln!("[obsidian] organize todos apply failed: {error:#}");
                }
                // 提交前再确认远端没动；动了说明人工又推了提交，放弃本次整理下轮重做。
                let vault = self.vault.clone();
                let inbox_dir = self.config.inbox_dir.clone();
                let expected = pending.target_sha.clone();
                let plan_snapshot = plan.clone();
                let outcome = tokio::task::spawn_blocking(move || -> Result<String> {
                    vault.fetch()?;
                    let head = vault.remote_head()?;
                    if head != expected {
                        vault.checkout_hard(&head)?;
                        return Ok("conflict".into());
                    }
                    // Agent 指令落盘：读-写-删经 Vault 执行（重试轮幂等，源缺失=已搬）。
                    let (applied, already) = execute_plan(vault.as_ref(), &plan_snapshot)?;
                    eprintln!(
                        "[obsidian] organize moves: {applied} applied, {already} already moved"
                    );
                    let committed = vault.commit_all(&format!(
                        "[ru41] organize inbox {}",
                        &expected[..expected.len().min(7)]
                    ))?;
                    if committed {
                        // 近 7 天整理去向索引：失败不影响整理产物推送。
                        if let Err(error) = update_organize_index(vault.as_ref(), &inbox_dir) {
                            eprintln!("[obsidian] organize index update failed: {error:#}");
                        }
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
                            let summary = plan_summary(&plan, pending.files.len());
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

    /// 把 Agent 识别的待办写入 TodoService；同标题的活跃待办去重，防重试轮重复入库。
    fn apply_plan_todos(&self, plan: &OrganizePlan) -> Result<()> {
        if plan.todos.is_empty() {
            return Ok(());
        }
        let options = self.todo_options.unwrap_or_default();
        let service = crate::app::todo::TodoService::new(self.store.clone(), options);
        let now = crate::util::now();
        let existing: std::collections::HashSet<String> = service
            .list(crate::domain::todo::TodoView::All, now, 500)?
            .into_iter()
            .filter(|item| item.status == crate::domain::todo::TodoStatus::Active)
            .map(|item| item.title)
            .collect();
        for todo in &plan.todos {
            let title = todo.title.trim();
            if title.is_empty() || existing.contains(title) {
                continue;
            }
            let due_at = todo
                .due
                .as_deref()
                .map(|text| crate::domain::todo::parse_due(text, now, &options))
                .transpose()?;
            service.add(
                crate::domain::todo::TodoInput {
                    title: title.to_owned(),
                    priority: crate::domain::todo::TodoPriority::Normal,
                    labels: vec!["obsidian".to_owned()],
                    due_at,
                    remind_at: due_at,
                    source: Some(crate::domain::todo::TodoSource {
                        channel: "obsidian".into(),
                        source_id: None,
                    }),
                },
                now,
            )?;
        }
        Ok(())
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
    pub(super) async fn handle_new_head(
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
        // 工作区对齐到目标提交后读取文件快照；正文先过红区替换再进 prompt，
        // vault 全文不出宿主——worker 里的 Agent 只看到脱敏快照。
        let vault = self.vault.clone();
        let sha = target.to_owned();
        tokio::task::spawn_blocking(move || vault.checkout_hard(&sha)).await??;
        let vault = self.vault.clone();
        let snapshot_files = files.clone();
        let snapshot: Vec<(String, Option<String>)> =
            tokio::task::spawn_blocking(move || -> Vec<(String, Option<String>)> {
                snapshot_files
                    .iter()
                    .map(|f| (f.clone(), vault.read_file(f).ok().flatten()))
                    .collect()
            })
            .await?;
        let snapshot: Vec<(String, Option<String>)> = snapshot
            .into_iter()
            .map(|(path, content)| {
                (
                    path,
                    content.map(|c| crate::privacy::scan::redact_secrets(&c)),
                )
            })
            .collect();
        let prompt = organize_prompt(&self.config.inbox_dir, &snapshot);
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

/// Agent 输出的整理指令（```ru41-organize 代码块内的 JSON）。
/// Agent 只产出指令，落盘由宿主经 Vault 执行——vault 全文与文件系统对 Agent 不可见。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OrganizePlan {
    #[serde(default)]
    pub moves: Vec<OrganizeMove>,
    #[serde(default)]
    pub deletes: Vec<String>,
    #[serde(default)]
    pub todos: Vec<OrganizeTodo>,
    #[serde(default)]
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrganizeMove {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrganizeTodo {
    pub title: String,
    /// YYYY-MM-DD 或自然语言；解析失败忽略该条截止。
    #[serde(default)]
    pub due: Option<String>,
}

const PLAN_FENCE: &str = "ru41-organize";

/// 从 Agent 输出提取整理指令：优先 ```ru41-organize 代码块，否则整段按 JSON 解析。
pub fn extract_plan(summary: &str) -> Result<OrganizePlan> {
    let json = if let Some(start) = summary.find(&format!("```{PLAN_FENCE}")) {
        let body_start = summary[start..]
            .find('\n')
            .map(|offset| start + offset + 1)
            .with_context(|| format!("{PLAN_FENCE} 代码块缺少换行"))?;
        let end = summary[body_start..]
            .find("```")
            .with_context(|| format!("{PLAN_FENCE} 代码块未闭合"))?
            + body_start;
        summary[body_start..end].trim()
    } else {
        summary.trim()
    };
    serde_json::from_str(json).context("整理指令 JSON 解析失败")
}

/// 路径校验：只接受 Vault 内相对路径，拒绝绝对路径、`..` 与空串。
fn ensure_vault_relative(path: &str) -> Result<()> {
    anyhow::ensure!(!path.trim().is_empty(), "空路径");
    let relative = std::path::Path::new(path);
    anyhow::ensure!(relative.is_relative(), "禁止绝对路径: {path}");
    anyhow::ensure!(
        relative
            .components()
            .all(|c| c != std::path::Component::ParentDir),
        "禁止 .. 路径: {path}"
    );
    Ok(())
}

/// 快照单文件/总量截断（字符数）：控制 prompt 体积，超限标记截断。
const SNAPSHOT_FILE_CHARS: usize = 20_000;
const SNAPSHOT_TOTAL_CHARS: usize = 60_000;

/// 快照收件箱 prompt：待整理文件的脱敏正文内联进 prompt，
/// Agent 不接触文件系统与 git，只输出 ru41-organize JSON 指令。
pub fn organize_prompt(inbox_dir: &str, files: &[(String, Option<String>)]) -> String {
    let mut out = format!(
        "整理 Obsidian Vault 收件箱：以下 {} 个 `{inbox_dir}/` 变更文件已由程序读取正文快照\n\
         （敏感信息已替换为 <规则名> 占位符，超长已截断）。你无需也无法访问文件系统。\n\n",
        files.len()
    );
    let mut used = 0usize;
    for (path, content) in files {
        let body = match content {
            None => "（非文本或读取失败：请把该文件列入 skipped）".to_owned(),
            Some(text) => {
                let mut text = text.trim_end().to_owned();
                if text.chars().count() > SNAPSHOT_FILE_CHARS
                    || used + text.chars().count() > SNAPSHOT_TOTAL_CHARS
                {
                    text = format!("{}\n…[已截断]", crate::util::truncate_chars(&text, 4_000));
                }
                used += text.chars().count();
                text
            }
        };
        out.push_str(&format!("### `{path}`\n\n```markdown\n{body}\n```\n\n"));
    }
    out.push_str(
        "## 任务\n\
         为每个文本文件选择 Vault 内合适的归档路径（课程笔记进 01-Courses-课程/，知识笔记进 \
         03-Knowledge-知识/ 等），给出移动指令；可规范化目标文件名。\n\n\
         规则：\n\
         - 只依据快照内容判断，不要臆造文件系统状态。\n\
         - 非文本、读取失败或无法判断归属的文件列入 `skipped`，保持原位。\n\
         - 快照里出现作业/待办事项（标题+截止日期）时输出 `todos`，`due` 用 YYYY-MM-DD。\n\
         - 只有「纯一句话待办」（全文 ≤2 行且待办已进 todos）允许列入 `deletes`；\
         其余文件一律只移动不删除。\n\
         - `from`/`to` 与 `deletes` 都是 Vault 内相对路径：不以 / 开头、不含 `..`。\n\
         - 不要执行任何 git 或文件操作，落盘由程序负责。\n\n\
         ## 输出格式（严格遵守）\n\
         回复只包含一个 ```ru41-organize 代码块，内含如下 JSON，不要输出其它内容：\n\
         ```ru41-organize\n\
         {\"moves\":[{\"from\":\"Inbox/a.md\",\"to\":\"01-Courses-课程/xxx/a.md\"}],\
         \"deletes\":[\"Inbox/一句话待办.md\"],\
         \"todos\":[{\"title\":\"交实验报告\",\"due\":\"2026-11-08\"}],\"skipped\":[\"Inbox/img.png\"]}\n\
         ```",
    );
    out
}

/// 从整理指令落盘（读-写-删经 Vault，纯文本）。已搬过/已删的源视为完成跳过，
/// 保证重试轮幂等。
fn execute_plan(vault: &dyn Vault, plan: &OrganizePlan) -> Result<(usize, usize)> {
    let mut applied = 0;
    let mut already = 0;
    for m in &plan.moves {
        ensure_vault_relative(&m.from)?;
        ensure_vault_relative(&m.to)?;
        match vault.read_file(&m.from)? {
            Some(content) => {
                vault.write_file(&m.to, &content)?;
                vault.remove_file(&m.from)?;
                applied += 1;
            }
            None => already += 1,
        }
    }
    for path in &plan.deletes {
        ensure_vault_relative(path)?;
        if vault.remove_file(path)? {
            applied += 1;
        }
    }
    Ok((applied, already))
}

/// 整理结果的人读摘要（QQ 通知用）：Agent 输出的是 JSON，直接转译。
fn plan_summary(plan: &OrganizePlan, file_count: usize) -> String {
    let mut lines = Vec::new();
    for m in &plan.moves {
        lines.push(format!("- `{}` → `{}`", m.from, m.to));
    }
    for t in &plan.todos {
        let due = t.due.as_deref().unwrap_or("无截止");
        lines.push(format!("- 待办：{}（{}）", t.title, due));
    }
    for s in &plan.skipped {
        lines.push(format!("- 保持原位：`{s}`"));
    }
    for d in &plan.deletes {
        lines.push(format!("- 已删除：`{d}`（纯一句话待办，已转待办）"));
    }
    if lines.is_empty() {
        return format!("Inbox 的 {file_count} 个变更已归档并推送，本地同步后可在 Obsidian 查看。");
    }
    lines.join("\n")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::vault::Vault;

    /// 内存 Vault fake：execute_plan 的幂等/校验测试用。
    struct FakeVault {
        files: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    }
    impl FakeVault {
        fn new(entries: &[(&str, &str)]) -> Self {
            Self {
                files: std::sync::Mutex::new(
                    entries
                        .iter()
                        .map(|(p, c)| (p.to_string(), c.to_string()))
                        .collect(),
                ),
            }
        }
    }
    fn stub<T>(v: T) -> anyhow::Result<T> {
        Ok(v)
    }

    impl Vault for FakeVault {
        fn remote_branch_sha(&self, _branch: &str) -> Result<Option<String>> {
            Ok(None)
        }
        fn push_head_to(&self, _remote_ref: &str, _force: bool) -> Result<()> {
            Ok(())
        }
        fn delete_remote_branch(&self, _branch: &str) -> Result<()> {
            Ok(())
        }
        fn head_sha(&self) -> Result<String> {
            Ok("0".repeat(40))
        }
        fn merge_commit(&self, _sha: &str) -> Result<crate::ports::vault::SyncOutcome> {
            Ok(crate::ports::vault::SyncOutcome::Clean)
        }
        fn merge_continue(&self) -> Result<()> {
            Ok(())
        }
        fn merge_abort(&self) -> Result<()> {
            Ok(())
        }
        fn conflict_files(&self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        fn is_ancestor(&self, _ancestor: &str, _other: &str) -> Result<bool> {
            Ok(true)
        }

        fn fetch(&self) -> Result<()> {
            Ok(())
        }
        fn remote_head(&self) -> Result<String> {
            stub("head".into())
        }
        fn head_subject(&self, _: &str) -> Result<String> {
            stub("subject".into())
        }
        fn changed_files(&self, _: &str, _: &str) -> Result<Vec<String>> {
            stub(vec![])
        }
        fn changed_entries(&self, _: &str, _: &str) -> Result<Vec<(String, bool)>> {
            stub(vec![])
        }
        fn checkout_hard(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn commit_all(&self, _: &str) -> Result<bool> {
            stub(true)
        }
        fn push(&self) -> Result<()> {
            Ok(())
        }
        fn sync(&self) -> Result<crate::ports::vault::SyncOutcome> {
            stub(crate::ports::vault::SyncOutcome::Clean)
        }
        fn resolve_conflict(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn rebase_continue(&self) -> Result<Vec<String>> {
            stub(vec![])
        }
        fn rebase_abort(&self) -> Result<()> {
            Ok(())
        }
        fn list_files(&self, _: &str, _: &str) -> Result<Vec<String>> {
            stub(vec![])
        }
        fn read_file_at(&self, _: &str, _: &str) -> Result<Option<String>> {
            stub(None)
        }
        fn read_file(&self, relative: &str) -> Result<Option<String>> {
            Ok(self.files.lock().unwrap().get(relative).cloned())
        }
        fn write_file(&self, relative: &str, content: &str) -> Result<()> {
            self.files
                .lock()
                .unwrap()
                .insert(relative.to_owned(), content.to_owned());
            Ok(())
        }
        fn write_bytes(&self, _: &str, _: &[u8]) -> Result<()> {
            Ok(())
        }
        fn remove_file(&self, relative: &str) -> Result<bool> {
            Ok(self.files.lock().unwrap().remove(relative).is_some())
        }
        fn append_inbox_note(&self, _: &str, _: &str, _: i64, _: i32) -> Result<String> {
            stub("Inbox/x.md".into())
        }
        fn recent_inbox_moves(
            &self,
            _: &str,
            _: u32,
        ) -> Result<Vec<crate::ports::vault::InboxMove>> {
            stub(vec![])
        }
    }

    #[test]
    fn extract_plan_from_fence_and_bare_json() {
        let fenced = "前言不该有\n```ru41-organize\n{\"moves\":[{\"from\":\"Inbox/a.md\",\"to\":\"01-Courses/a.md\"}],\"todos\":[],\"skipped\":[]}\n```\n";
        let plan = extract_plan(fenced).unwrap();
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].to, "01-Courses/a.md");
        assert!(plan.deletes.is_empty());

        let bare = "{\"moves\":[],\"deletes\":[\"Inbox/b.md\"],\"todos\":[{\"title\":\"交作业\",\"due\":\"2026-11-08\"}],\"skipped\":[]}";
        let plan = extract_plan(bare).unwrap();
        assert_eq!(plan.deletes, vec!["Inbox/b.md".to_owned()]);
        assert_eq!(plan.todos[0].due.as_deref(), Some("2026-11-08"));

        assert!(extract_plan("Agent 忘了按格式输出").is_err());
        assert!(extract_plan("```ru41-organize\n{\"moves\":[]}\n``` 缺闭合").is_ok());
    }

    #[test]
    fn vault_relative_paths_are_enforced() {
        assert!(ensure_vault_relative("01-Courses/a.md").is_ok());
        assert!(ensure_vault_relative("Inbox/../secret.md").is_err());
        assert!(ensure_vault_relative("/etc/passwd").is_err());
        assert!(ensure_vault_relative("  ").is_err());
        assert!(ensure_vault_relative("a/../../b.md").is_err());
    }

    #[test]
    fn prompt_inlines_snapshot_with_truncation_markers() {
        let long = "x".repeat(30_000);
        let files = vec![
            (
                "Inbox/a.md".to_owned(),
                Some("笔记正文 password=<secret_kv> 结束".to_owned()),
            ),
            ("Inbox/long.md".to_owned(), Some(long.clone())),
            ("Inbox/img.png".to_owned(), None),
        ];
        let prompt = organize_prompt("00-Inbox-收件箱", &files);
        assert!(prompt.contains("Inbox/a.md"));
        assert!(prompt.contains("password=<secret_kv>"));
        assert!(prompt.contains("…[已截断]"));
        assert!(prompt.contains("列入 skipped"));
        assert!(prompt.contains("ru41-organize"));
        assert!(!prompt.contains(&long), "超长正文不整体进 prompt");
        assert!(!prompt.contains("vault_path"));
    }

    #[test]
    fn execute_plan_applies_moves_deletes_and_is_idempotent() {
        let vault = FakeVault::new(&[("Inbox/a.md", "内容A"), ("Inbox/一句话.md", "记得交作业")]);
        let plan = OrganizePlan {
            moves: vec![OrganizeMove {
                from: "Inbox/a.md".into(),
                to: "03-Knowledge-知识/a.md".into(),
            }],
            deletes: vec!["Inbox/一句话.md".into()],
            todos: vec![],
            skipped: vec![],
        };
        let (applied, already) = execute_plan(&vault, &plan).unwrap();
        assert_eq!((applied, already), (2, 0));
        let files = vault.files.lock().unwrap();
        assert!(!files.contains_key("Inbox/a.md"));
        assert_eq!(
            files.get("03-Knowledge-知识/a.md").map(String::as_str),
            Some("内容A")
        );
        assert!(!files.contains_key("Inbox/一句话.md"));
        drop(files);
        // 重试轮：源已不在 → 全部视为已完成，不报错。
        let (applied, already) = execute_plan(&vault, &plan).unwrap();
        assert_eq!((applied, already), (0, 1));
    }
}
