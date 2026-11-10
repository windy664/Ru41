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
        let vault = Self {
            path: path.into(),
            remote: remote.into(),
            branch: branch.into(),
        };
        vault.disable_background_maintenance();
        vault
    }

    /// 关掉 git 的「后台自动维护」。
    ///
    /// git 的自动 gc/maintenance 是 **detach** 出去的：父进程退出后它被 reparent 到
    /// PID 1（容器里就是我们自己），没人回收 → 每次 merge 漏一个僵尸进程；
    /// 而它干的是 repack——多核 CPU 打满 + 大量磁盘 I/O 的尖峰。
    ///
    /// 2026-11-07 实测：笔记本无电池（BAT0 的 energy_now 直接 No such device），
    /// 纯适配器供电；后台 repack 的负载尖峰让适配器跌落 → 整机瞬间断电，
    /// 内核连日志都来不及写（无关机序列、无 OOM、无 panic），AC 恢复后 BIOS 自动开机。
    ///
    /// 仓库很小（几百文件），松散对象靠定时手动 gc 就行，不必每次 merge 都自动 repack。
    fn disable_background_maintenance(&self) {
        for (key, value) in [
            ("gc.auto", "0"),
            ("maintenance.auto", "false"),
            ("gc.autoDetach", "false"),
        ] {
            let _ = Command::new("git")
                .args(["config", key, value])
                .current_dir(&self.path)
                .env("GIT_TELEMETRY", "0")
                .output();
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

    /// 提交工作区里的全部改动。调用方必须已持有仓库锁（flock 不可重入）。
    ///
    /// 没有东西可提交时返回 false，不产生空提交。
    fn commit_all_locked(&self, message: &str) -> Result<bool> {
        self.git(&["add", "-A"])?;
        let status = Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(&self.path)
            .status()
            .context("cannot run git diff")?;
        if status.success() {
            return Ok(false);
        }
        self.git(&["commit", "-m", message])?;
        Ok(true)
    }
}

/// 原子写：同目录临时文件 + rename。
///
/// 文件级同步（Syncthing 等）盯着目录变化，非原子写可能被同步到半截文件；
/// rename 在同一文件系统内是原子的，读者只会看到旧版或新版。
fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tmp".to_owned());
    let tmp = parent.join(format!(".{name}.ru41-tmp"));
    std::fs::write(&tmp, data)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&tmp);
            Err(error).with_context(|| format!("cannot rename into {}", path.display()))
        }
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
        self.locked(|vault| vault.commit_all_locked(message))
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
            // 工作区不干净时 `git pull --rebase` 直接拒绝：
            //   error: cannot pull with rebase: You have unstaged changes.
            // 而 vault 是所有插件共写的：projects / chaoxing / zhihuishu 随时会
            // 往里写文件，且常常要跑好几分钟才提交一次（projects 扫 117 个仓库、
            // GitHub 免鉴权配额被打满，一轮 8 分钟）。obsidian 每 5 分钟轮询一次，
            // 于是几乎每次都撞进那个脏窗口——每轮白费一次 fetch + 一次失败的
            // rebase，还让本该 fast-forward 的同步退化成 fetch + merge --no-ff。
            //
            // 先把本地改动提交掉再拉：这些文件本来就是机器生成的，提交内容跟
            // 插件自己那条 "[ru41] …" 提交没有区别。
            vault.commit_all_locked("[ru41] 同步前提交")?;
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

    fn read_bytes(&self, relative: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path.join(relative);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(std::fs::read(&path)?))
    }

    fn write_file(&self, relative: &str, content: &str) -> Result<()> {
        self.locked(|vault| {
            let path = vault.path.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            atomic_write(&path, content.as_bytes())?;
            Ok(())
        })
    }

    fn write_bytes(&self, relative: &str, data: &[u8]) -> Result<()> {
        self.locked(|vault| {
            let path = vault.path.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            atomic_write(&path, data)?;
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

    fn remote_branch_sha(&self, branch: &str) -> Result<Option<String>> {
        // for-each-ref：分支不存在时退出码仍是 0（输出为空），
        // rev-parse --verify 会以 1 退出，不能直接用。
        let out = self.git(&[
            "for-each-ref",
            "--format=%(objectname)",
            &format!("refs/remotes/{}/{branch}", self.remote),
        ])?;
        let sha = out.lines().next().unwrap_or("").trim();
        if sha.is_empty() {
            return Ok(None);
        }
        Ok(Some(sha.to_owned()))
    }

    fn push_head_to(&self, remote_ref: &str, force: bool) -> Result<()> {
        self.locked(|vault| {
            let mut args = vec!["push"];
            if force {
                args.push("--force");
            }
            args.push(&vault.remote);
            let target = format!("HEAD:{remote_ref}");
            args.push(&target);
            vault.git(&args).map(|_| ())
        })
    }

    fn delete_remote_branch(&self, branch: &str) -> Result<()> {
        self.locked(|vault| {
            let output = Command::new("git")
                .args(["push", &vault.remote, "--delete", branch])
                .current_dir(&vault.path)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("LC_ALL", "C")
                .output()
                .context("cannot run git")?;
            if !output.status.success() {
                let error = String::from_utf8_lossy(&output.stderr);
                if !error.contains("remote ref does not exist") {
                    bail!("git push failed: {}", error.trim());
                }
            }
            // SSH 降级会把待合并提交直接写进这个 remote-tracking ref；
            // Gitee 上没有对应分支时也要把它清掉，避免下一轮重复合并。
            vault
                .git(&[
                    "update-ref",
                    "-d",
                    &format!("refs/remotes/{}/{branch}", vault.remote),
                ])
                .map(|_| ())
        })
    }

    fn head_sha(&self) -> Result<String> {
        self.git(&["rev-parse", "HEAD"])
    }

    fn merge_commit(&self, sha: &str) -> Result<SyncOutcome> {
        self.locked(|vault| {
            // --no-ff：保留双方提交（本地 [local] 提交原样进 master，历史可追溯），
            // 且合并结果一眼能看出是「本地改动并入」。
            if let Err(error) = vault.git(&["merge", "--no-ff", "--no-edit", sha]) {
                let conflicts = vault.conflict_files().unwrap_or_default();
                if conflicts.is_empty() {
                    let _ = vault.git(&["merge", "--abort"]);
                    return Err(error).context("vault merge failed (aborted)");
                }
                return Ok(SyncOutcome::Conflict(conflicts));
            }
            Ok(SyncOutcome::Clean)
        })
    }

    fn merge_continue(&self) -> Result<()> {
        self.locked(|vault| vault.git(&["commit", "--no-edit"]).map(|_| ()))
    }

    fn merge_abort(&self) -> Result<()> {
        self.locked(|vault| vault.git(&["merge", "--abort"]).map(|_| ()))
    }

    fn conflict_files(&self) -> Result<Vec<String>> {
        self.conflict_files()
    }

    fn is_ancestor(&self, ancestor: &str, other: &str) -> Result<bool> {
        let output = Command::new("git")
            .args(["merge-base", "--is-ancestor", ancestor, other])
            .current_dir(&self.path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .context("cannot run git merge-base")?;
        // 退出码 0 = 是祖先；1 = 不是；其他 = 真错误。
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => bail!(
                "git merge-base failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
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
