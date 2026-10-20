use crate::ports::vault::Vault;
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
