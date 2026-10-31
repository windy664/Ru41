use crate::ports::vault::{InboxMove, SyncOutcome, Vault};
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 空树 SHA（git 内置常量）：尚无基线时用它做 diff 起点，等价于「所有文件都是新增」。
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Vault 的 git 工作副本实现。全部是同步阻塞调用：
/// 长驻轮询由调度层包 spawn_blocking，QQ 命令路径在 block_in_place 上下文中。
/// 凭证不进配置：git credential helper（如 credential.helper=store）自行解决。
///
/// 写操作（checkout/commit/push/写文件/追加笔记）在进入前取 `.git/ru41.lock` 的排他锁：
/// QQ 记一下（qq 容器）与 Vault 轮询（obsidian 容器）是同一工作区的两个进程，必须串行。
pub struct GitVault {
    path: PathBuf,
    remote: String,
    branch: String,
}

impl GitVault {
    pub fn new(
        path: impl Into<PathBuf>,
        remote: impl Into<String>,
        branch: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            remote: remote.into(),
            branch: branch.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        // core.quotepath=false：中文路径按原始 UTF-8 输出，否则 diff 结果无法按目录前缀匹配。
        let output = Command::new("git")
            .arg("-c")
            .arg("core.quotepath=false")
            .args(args)
            .current_dir(&self.path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .context("cannot run git")?;
        if !output.status.success() {
            bail!(
                "git {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// rebase 冲突中的文件列表（UU/AA 等未解决路径）。
    fn conflict_files(&self) -> Result<Vec<String>> {
        let out = self.git(&["diff", "--name-only", "--diff-filter=U"])?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// 对仓库级写操作取排他文件锁（flock，跨进程，同文件系统有效）。
    fn locked<R>(&self, op: impl FnOnce(&Self) -> Result<R>) -> Result<R> {
        let lock_path = self.path.join(".git/ru41.lock");
        let file = File::create(&lock_path)
            .with_context(|| format!("cannot create {}", lock_path.display()))?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            bail!("cannot lock {}", lock_path.display());
        }
        op(self)
    }
}

impl Vault for GitVault {
    fn fetch(&self) -> Result<()> {
        self.git(&["fetch", &self.remote]).map(|_| ())
    }

    fn remote_head(&self) -> Result<String> {
        self.git(&["rev-parse", &format!("{}/{}", self.remote, self.branch)])
    }

    fn head_subject(&self, sha: &str) -> Result<String> {
        self.git(&["log", "-1", "--format=%s", sha])
    }

    fn changed_files(&self, from: &str, to: &str) -> Result<Vec<String>> {
        let out = self.git(&["diff", "--name-only", from, to])?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn changed_entries(&self, from: &str, to: &str) -> Result<Vec<(String, bool)>> {
        let out = self.git(&["diff", "--name-status", "--no-renames", from, to])?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let (status, path) = line.split_once('\t')?;
                let path = path.trim();
                if path.is_empty() {
                    return None;
                }
                Some((path.to_owned(), status.trim() == "D"))
            })
            .collect())
    }

    fn list_files(&self, at: &str, prefix: &str) -> Result<Vec<String>> {
        let out = self.git(&["ls-tree", "-r", "--name-only", at, "--", prefix])?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn read_file_at(&self, at: &str, relative: &str) -> Result<Option<String>> {
        let output = Command::new("git")
            .args(["show", &format!("{at}:{relative}")])
            .current_dir(&self.path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .context("cannot run git show")?;
        if !output.status.success() {
            // 路径不存在 / 非文件统一按 None；其他错误（仓库损坏等）会在下轮暴露。
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    }

    fn checkout_hard(&self, sha: &str) -> Result<()> {
        self.locked(|vault| vault.git(&["reset", "--hard", sha]).map(|_| ()))
    }

    fn commit_all(&self, message: &str) -> Result<bool> {
        self.locked(|vault| {
            vault.git(&["add", "-A"])?;
            let status = Command::new("git")
                .args(["diff", "--cached", "--quiet"])
                .current_dir(&vault.path)
                .status()
                .context("cannot run git diff")?;
            if status.success() {
                return Ok(false);
            }
            vault.git(&["commit", "-m", message])?;
            Ok(true)
        })
    }

    fn push(&self) -> Result<()> {
        self.locked(|vault| {
            vault
                .git(&["push", &vault.remote, &format!("HEAD:{}", vault.branch)])
                .map(|_| ())
        })
    }

    fn sync(&self) -> Result<SyncOutcome> {
        self.locked(|vault| {
            if let Err(error) = vault.git(&["pull", "--rebase", &vault.remote, &vault.branch]) {
                let conflicts = vault.conflict_files().unwrap_or_default();
                if conflicts.is_empty() {
                    // 非冲突失败（网络、认证等）：中止 rebase，恢复原状。
                    let _ = vault.git(&["rebase", "--abort"]);
                    return Err(error).context("vault sync rebase failed (aborted)");
                }
                // 冲突现场保留，交给调用方（AI 合并）处理。
                return Ok(SyncOutcome::Conflict(conflicts));
            }
            let ahead = vault.git(&[
                "rev-list",
                "--count",
                &format!("{}/{}..HEAD", vault.remote, vault.branch),
            ])?;
            if ahead.trim() == "0" {
                return Ok(SyncOutcome::Clean);
            }
            vault.git(&["push", &vault.remote, &format!("HEAD:{}", vault.branch)])?;
            Ok(SyncOutcome::Pushed)
        })
    }

    fn resolve_conflict(&self, relative: &str, merged: &str) -> Result<()> {
        self.locked(|vault| {
            let path = vault.path.join(relative);
            std::fs::write(&path, merged)
                .with_context(|| format!("cannot write {}", path.display()))?;
            vault.git(&["add", "--", relative])?;
            Ok(())
        })
    }

    fn rebase_continue(&self) -> Result<Vec<String>> {
        self.locked(|vault| {
            // core.editor=true：合并提交信息沿用原文，不弹编辑器。
            match vault.git(&["-c", "core.editor=true", "rebase", "--continue"]) {
                Ok(_) => Ok(Vec::new()),
                Err(error) => {
                    let conflicts = vault.conflict_files().unwrap_or_default();
                    if !conflicts.is_empty() {
                        // 下一个提交又冲突：返回新的冲突清单继续解。
                        return Ok(conflicts);
                    }
                    let msg = format!("{error:#}");
                    if msg.contains("No changes") || msg.contains("nothing to commit") {
                        // 合并结果与上游一致，该提交为空：跳过。
                        vault.git(&["rebase", "--skip"])?;
                        return Ok(Vec::new());
                    }
                    Err(error.context("vault rebase --continue failed"))
                }
            }
        })
    }

    fn rebase_abort(&self) -> Result<()> {
        self.locked(|vault| vault.git(&["rebase", "--abort"]).map(|_| ()))
    }

    fn read_file(&self, relative: &str) -> Result<Option<String>> {
        let path = self.path.join(relative);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(&path)?))
    }

    fn write_file(&self, relative: &str, content: &str) -> Result<()> {
        self.locked(|vault| {
            let path = vault.path.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, content)?;
            Ok(())
        })
    }

    fn write_bytes(&self, relative: &str, data: &[u8]) -> Result<()> {
        self.locked(|vault| {
            let path = vault.path.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, data)?;
            Ok(())
        })
    }

    fn remove_file(&self, relative: &str) -> Result<bool> {
        self.locked(|vault| {
            let path = vault.path.join(relative);
            if !path.exists() {
                return Ok(false);
            }
            std::fs::remove_file(&path)?;
            Ok(true)
        })
    }

    fn recent_inbox_moves(&self, inbox_dir: &str, days: u32) -> Result<Vec<InboxMove>> {
        let inbox_prefix = format!("{inbox_dir}/");
        // -M 开重命名检测：agent 归档是「旧路径删 + 新路径增」，相似内容会被识别为 R。
        // %x1e 分隔提交记录，避免中文路径/多行提交说明干扰解析。
        let out = self.git(&[
            "log",
            &format!("--since={days}.days"),
            "--date=short",
            "--format=%x1e%ad",
            "--name-status",
            "-M",
        ])?;
        let mut moves: Vec<InboxMove> = Vec::new();
        for record in out.split('\x1e') {
            let mut lines = record.lines();
            let Some(date) = lines.next().map(str::trim).filter(|d| !d.is_empty()) else {
                continue;
            };
            for line in lines {
                let mut fields = line.split('\t');
                let status = fields.next().unwrap_or_default().trim();
                let (source, destination) = if status.starts_with('R') {
                    match (fields.next(), fields.next()) {
                        (Some(from), Some(to)) => (from, to.to_owned()),
                        _ => continue,
                    }
                } else if status == "D" {
                    match fields.next() {
                        Some(from) => (from, "已删除（可能已转待办或合并进其他笔记）".to_owned()),
                        None => continue,
                    }
                } else {
                    continue;
                };
                let Some(name) = source.strip_prefix(inbox_prefix.as_str()) else {
                    continue;
                };
                // 收件箱内部改名（如 [待分类] 前缀调整）与程序自身文件不算归档。
                if destination.starts_with(&inbox_prefix)
                    || name == "快速记录.md"
                    || name == "整理索引.md"
                {
                    continue;
                }
                moves.push(InboxMove {
                    date: date.to_owned(),
                    name: name.to_owned(),
                    destination,
                });
            }
        }
        // 同名文件只留最近一次去向：多次转手时用户只关心最终归宿。
        moves.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.name.cmp(&b.name)));
        let mut seen = std::collections::HashSet::new();
        moves.retain(|m| seen.insert(m.name.clone()));
        Ok(moves)
    }

    fn append_inbox_note(
        &self,
        inbox_dir: &str,
        text: &str,
        now: i64,
        offset_seconds: i32,
    ) -> Result<String> {
        self.locked(|vault| {
            let relative = format!("{inbox_dir}/快速记录.md");
            let path = vault.path.join(&relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if !path.exists() {
                std::fs::write(&path, "# 快速记录\n\n")?;
            }
            let options = crate::domain::todo::TodoOptions {
                offset_seconds,
                recent_days: 7,
                long_term_days: 14,
            };
            let line = format!(
                "- {} {}\n",
                crate::domain::todo::due_label(Some(now), now, options, false),
                text.trim()
            );
            let mut content = std::fs::read_to_string(&path)?;
            content.push_str(&line);
            std::fs::write(&path, content)?;
            Ok(relative)
        })
    }
}
