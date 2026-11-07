//! git 同步与冲突合并：rebase 冲突 AI 合并、sync-conflict 副本合并、
//! 远端待合并分支（PR 模型）并入 master。

use super::ObsidianService;
use anyhow::{Context, Result, bail};

/// 本地电脑推送待合并内容的远端分支名（PR 模型的「待合并分支」）。
pub const PENDING_BRANCH: &str = "ru41/local";

/// 一个同步冲突副本：副本路径 + 原文件路径（原文件可能被另一端删了）。
struct SyncConflictFile {
    path: std::path::PathBuf,
    original_path: Option<std::path::PathBuf>,
}

/// 在 vault 目录里找同步冲突副本（`名字.sync-conflict-<时间戳>-<设备>.md`），
/// 配对原文件（原文件可能已被另一端删除，此时不配对）。
fn find_sync_conflicts(root: &std::path::Path) -> Result<Vec<SyncConflictFile>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !name.contains(".sync-conflict-") {
                continue;
            }
            let Some((stem, rest)) = name.split_once(".sync-conflict-") else {
                continue;
            };
            if stem.is_empty() {
                continue;
            }
            // 副本名是「原名.sync-conflict-<时间戳>-<设备>.扩展名」，
            // 原文件名 = 前缀 + 扩展名。
            let extension = std::path::Path::new(rest)
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            let original = path.with_file_name(format!("{stem}{extension}"));
            found.push(SyncConflictFile {
                path: path.clone(),
                original_path: if original.exists() {
                    Some(original)
                } else {
                    None
                },
            });
        }
    }
    Ok(found)
}

impl ObsidianService {
    /// 扫描 vault 里的同步冲突副本（`*.sync-conflict-*`），逐个自动合并回原文件。
    ///
    /// 文件级同步（Syncthing 等）撞车时的语义是「一份留原名，另一份另存为
    /// `名字.sync-conflict-<时间戳>-<设备>.md`」——数据不丢，但会留下怪文件。
    /// 这里把两份交给 AI 语义合并，写回原文件、删掉副本，再统一提交推送。
    /// 没有合并能力（无 key）时只通知，不动文件。
    pub(super) async fn resolve_sync_conflicts(&self) -> Result<Vec<String>> {
        let conflicts = self.find_sync_conflicts()?;
        if conflicts.is_empty() {
            return Ok(Vec::new());
        }
        let Some(settings) = self.config.merge.clone() else {
            let items: Vec<String> = conflicts
                .iter()
                .map(|c| c.path.to_string_lossy().into_owned())
                .collect();
            self.notify_conflicts_found(
                &items,
                Some("## 发现同步冲突副本，但 AI 合并未启用，需要人工看一眼："),
            )
            .await?;
            return Ok(Vec::new());
        };
        let resolver = crate::app::merge::MergeResolver::new(
            &settings.base_url,
            &settings.model,
            &settings.api_key,
            settings.api,
        );
        let mut merged = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        for conflict in &conflicts {
            let Some(original) = conflict.original_path.clone() else {
                continue;
            };
            let original_rel = original
                .strip_prefix(&self.config.vault_path)
                .map(|p| p.to_string_lossy().trim_start_matches('/').to_owned())
                .unwrap_or_else(|_| original.to_string_lossy().into_owned());
            let original_text = match tokio::task::spawn_blocking({
                let path = original.clone();
                move || std::fs::read_to_string(&path)
            })
            .await?
            {
                Ok(text) => text,
                Err(error) => {
                    failures.push(format!("{original_rel}：读不到原文件（{error}）"));
                    continue;
                }
            };
            let other_text = match tokio::task::spawn_blocking({
                let path = conflict.path.clone();
                move || std::fs::read_to_string(&path)
            })
            .await?
            {
                Ok(text) => text,
                Err(error) => {
                    failures.push(format!("{original_rel}：读不到冲突副本（{error}）"));
                    continue;
                }
            };
            match resolver
                .resolve_versions(&original_rel, &original_text, &other_text)
                .await
            {
                Ok(text) => {
                    let path = original.clone();
                    let body = text.clone();
                    tokio::task::spawn_blocking(move || -> Result<()> {
                        std::fs::write(&path, &body)?;
                        Ok(())
                    })
                    .await??;
                    let path = conflict.path.clone();
                    tokio::task::spawn_blocking(move || -> Result<()> {
                        std::fs::remove_file(&path)?;
                        Ok(())
                    })
                    .await??;
                    merged.push(original_rel);
                }
                Err(error) => failures.push(format!("{original_rel}：{error:#}")),
            }
        }
        if !merged.is_empty() {
            let vault = self.vault.clone();
            let committed = tokio::task::spawn_blocking(move || {
                vault.commit_all("[ru41] 合并同步冲突副本")?;
                vault.push()?;
                Ok::<bool, anyhow::Error>(true)
            })
            .await??;
            let _ = committed;
            self.notify_conflicts_found(&merged, None).await?;
        }
        if !failures.is_empty() {
            self.notify_conflicts_found(&failures, Some("以下冲突没能自动合并，需要人工看一眼："))
                .await?;
        }
        Ok(merged)
    }

    /// 在 vault 里找同步冲突副本，返回 (副本路径, 原文件路径)。
    fn find_sync_conflicts(&self) -> Result<Vec<SyncConflictFile>> {
        find_sync_conflicts(&self.config.vault_path)
    }

    /// 冲突副本通知：已合并的与合不掉的分开说。
    async fn notify_conflicts_found(&self, items: &[String], header: Option<&str>) -> Result<()> {
        let Some(notifier) = &self.notifier else {
            return Ok(());
        };
        let list = items
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        let title = header.unwrap_or("## 同步冲突副本已自动合并");
        notifier
            .send(
                &self.notify_target,
                None,
                &format!(
                    "{title}\n\n{list}\n\n> 冲突副本已合并回原文件并删除；请抽空过目合并质量。"
                ),
            )
            .await?;
        Ok(())
    }

    /// 远端合并队列：把本地电脑推来的待合并分支并进 master。
    ///
    /// 本地电脑和服务器都是写者，却共用一个分支：本地直接推 master 会被拒，
    /// rebase 又会撞冲突（冲突标记会插进笔记正文，而本地没有 AI 合并能力）。
    /// 改用 PR 模型——本地把自己的提交推到「待合并分支」，服务器 merge 进 master，
    /// 冲突由这里已有的 AI 语义合并解决，合并后删掉待合并分支。
    /// 本地随后把 master 取回（reset --hard），全程本地不做 merge/rebase。
    ///
    /// 返回并入的提交数。
    pub(super) async fn merge_remote_pending(&self) -> Result<u32> {
        let branch = PENDING_BRANCH;
        let vault = self.vault.clone();
        // fetch 后远端分支的 SHA 才能读到。
        let pending_sha = tokio::task::spawn_blocking({
            let vault = vault.clone();
            let branch = branch.to_owned();
            move || -> Result<Option<String>> {
                vault.fetch()?;
                vault.remote_branch_sha(&branch)
            }
        })
        .await??;
        let Some(pending_sha) = pending_sha else {
            return Ok(0);
        };
        // 已经在 master 里（上一轮合过）→ 只清掉远端分支，不重复合并。
        let already = tokio::task::spawn_blocking({
            let vault = vault.clone();
            let sha = pending_sha.clone();
            move || -> Result<bool> { vault.is_ancestor(&sha, "HEAD") }
        })
        .await??;
        if already {
            let vault = self.vault.clone();
            tokio::task::spawn_blocking(move || vault.delete_remote_branch(branch)).await??;
            return Ok(0);
        }
        // master 的最新 SHA 作为合并基准。
        let master_sha = tokio::task::spawn_blocking({
            let vault = self.vault.clone();
            move || vault.remote_head()
        })
        .await??;
        let _ = master_sha;
        let vault = self.vault.clone();
        let pending = pending_sha.clone();
        let outcome = tokio::task::spawn_blocking(move || vault.merge_commit(&pending)).await??;
        // 冲突由 AI 逐文件语义合并后继续（复用已有的合并器）。
        let conflicts = match outcome {
            crate::ports::vault::SyncOutcome::Conflict(files) => files,
            _ => Vec::new(),
        };
        if !conflicts.is_empty()
            && let Err(error) = self.merge_pending_conflicts(&conflicts).await
        {
            eprintln!("[obsidian] pending merge failed: {error:#}");
            let vault = self.vault.clone();
            let _ = tokio::task::spawn_blocking(move || vault.merge_abort()).await;
            return Ok(0);
        }
        // 推送合并结果并清掉待合并分支——但只在分支仍指向刚合并的那个提交时才删：
        // 本地在这期间推了新提交上来，删了就等于把人家的工作扔了（下一轮会重推，
        // 但那要等一个轮询周期）。指向别的提交就留着，下一轮继续合。
        let vault = self.vault.clone();
        let pending_for_log = pending_sha.clone();
        let pending = pending_sha.clone();
        let branch = branch.to_owned();
        tokio::task::spawn_blocking(move || -> Result<()> {
            vault.push()?;
            vault.fetch()?;
            let current = vault.remote_branch_sha(&branch)?;
            if current.as_deref() == Some(pending.as_str()) {
                vault.delete_remote_branch(&branch)?;
            } else {
                eprintln!(
                    "[obsidian] pending branch moved while merging ({} → {current:?}), keep it",
                    &pending[..pending.len().min(8)]
                );
            }
            Ok(())
        })
        .await??;
        eprintln!("[obsidian] merged remote pending {pending_for_log} into master");
        Ok(1)
    }

    /// 待合并分支的冲突 → AI 逐文件语义合并 → 完成合并提交。
    async fn merge_pending_conflicts(&self, files: &[String]) -> Result<()> {
        let settings = self
            .config
            .merge
            .clone()
            .context("AI 合并未启用或无 key（[agent.pi] 供应商配置）")?;
        let resolver = crate::app::merge::MergeResolver::new(
            &settings.base_url,
            &settings.model,
            &settings.api_key,
            settings.api,
        );
        for file in files {
            let vault = self.vault.clone();
            let path = file.clone();
            let conflicted = tokio::task::spawn_blocking(move || vault.read_file(&path))
                .await??
                .with_context(|| format!("冲突文件读取失败 {file}"))?;
            let merged = resolver.resolve(file, &conflicted).await?;
            let vault = self.vault.clone();
            let path = file.clone();
            tokio::task::spawn_blocking(move || vault.resolve_conflict(&path, &merged)).await??;
        }
        let vault = self.vault.clone();
        tokio::task::spawn_blocking(move || vault.merge_continue()).await??;
        Ok(())
    }

    /// rebase 冲突现场 → AI 逐文件语义合并 → continue → push → QQ 告知。
    /// 任何一步失败都 Err，由调用方 abort 现场并通知人工。
    pub(super) async fn merge_conflicts(&self, files: &[String]) -> Result<()> {
        let settings = self
            .config
            .merge
            .clone()
            .context("AI 合并未启用或无 key（obsidian.merge_* / MINIMAX_API_KEY）")?;
        if let Some(bad) = files
            .iter()
            .find(|f| !crate::app::merge::is_mergeable_path(f))
        {
            bail!("含非文本冲突文件 {bad}，交人工处理");
        }
        let resolver = crate::app::merge::MergeResolver::new(
            &settings.base_url,
            &settings.model,
            &settings.api_key,
            settings.api,
        );
        let mut pending: Vec<String> = files.to_vec();
        // 一批解完 continue 后，下一个提交可能再冲突；封顶 8 批防死循环。
        for _ in 0..8 {
            for file in &pending {
                let vault = self.vault.clone();
                let path = file.clone();
                let conflicted = tokio::task::spawn_blocking(move || vault.read_file(&path))
                    .await??
                    .with_context(|| format!("冲突文件读取失败 {file}"))?;
                let merged = resolver.resolve(file, &conflicted).await?;
                let vault = self.vault.clone();
                let path = file.clone();
                tokio::task::spawn_blocking(move || vault.resolve_conflict(&path, &merged))
                    .await??;
            }
            let vault = self.vault.clone();
            pending = tokio::task::spawn_blocking(move || vault.rebase_continue()).await??;
            if pending.is_empty() {
                let vault = self.vault.clone();
                tokio::task::spawn_blocking(move || vault.push()).await??;
                self.notify_merged(files).await?;
                return Ok(());
            }
        }
        bail!("冲突批次数超过上限（8），交人工处理")
    }

    /// AI 合并成功通知：合并质量需用户抽查，附上文件清单。
    async fn notify_merged(&self, files: &[String]) -> Result<()> {
        let Some(notifier) = &self.notifier else {
            return Ok(());
        };
        let list = files
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        notifier
            .send(
                &self.notify_target,
                None,
                &format!(
                    "## Vault 同步冲突已自动合并\n\n{} 个冲突文件已由 AI 语义合并并推送：\n{list}\n\n> 请抽空过目合并质量；如有误合可直接在 vault 里改，下一轮同步会带走。",
                    files.len()
                ),
            )
            .await?;
        Ok(())
    }

    /// AI 合并失败通知：现场已 abort 还原，需人工处理。
    pub(super) async fn notify_merge_failed(
        &self,
        files: &[String],
        error: &anyhow::Error,
    ) -> Result<()> {
        let Some(notifier) = &self.notifier else {
            return Ok(());
        };
        let list = files
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        notifier
            .send(
                &self.notify_target,
                None,
                &format!(
                    "## Vault 同步冲突需人工处理\n\nAI 自动合并失败：{}\n\n冲突文件：\n{list}\n\n> 同步现场已回滚，可手动进 vault 仓库 `git pull --rebase` 处理。",
                    crate::util::truncate_chars(&format!("{error:#}"), 300)
                ),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sync_conflict_copies_and_pairs_them_with_original() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("05-Opportunities-机会")).unwrap();
        std::fs::write(root.join("05-Opportunities-机会/笔记.md"), "原内容").unwrap();
        std::fs::write(
            root.join("05-Opportunities-机会/笔记.sync-conflict-20261106-101010-ABCD.md"),
            "副本内容",
        )
        .unwrap();
        // 没有原文件的孤立副本不算配对。
        std::fs::write(
            root.join("05-Opportunities-机会/孤儿.sync-conflict-20261106-101010-X.md"),
            "孤儿",
        )
        .unwrap();

        let found = find_sync_conflicts(root).unwrap();
        assert_eq!(found.len(), 2);
        let paired = found.iter().filter(|c| c.original_path.is_some()).count();
        assert_eq!(paired, 1);
    }
}
