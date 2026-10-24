//! 项目库管理：扫描 02-Projects 下的项目笔记，对有 `repo:`  frontmatter 的
//! GitHub 项目同步 README 与元数据，让 vault 里的项目目录始终带着仓库现状。
//!
//! 读写纪律：扫描/读笔记走文件系统直读（同 obsidian 索引器）；写 vault 必须
//! 经 `Vault` 端口（单一写入者，git 闭环）。项目笔记是人写的，机器只拥有
//! `Repo-GitHub.md` 一个文件。

use crate::adapters::sources::github::{GithubClient, GithubRepoMeta};
use crate::ports::vault::Vault;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// projects 同步运行时配置（由 ProjectsSection::resolved 解析产出）。
#[derive(Debug, Clone)]
pub struct ProjectsConfig {
    /// vault 根（容器内挂载路径），直读扫描用。
    pub vault_root: PathBuf,
    /// Vault 内项目库根目录（相对路径）。
    pub projects_dir: String,
    pub interval_secs: u64,
}

/// 一个项目目录 + 它的 GitHub 锚点。
#[derive(Debug, Clone)]
pub struct ProjectEntry {
    /// 目录名即项目名。
    pub name: String,
    /// 相对 vault 的项目目录路径。
    pub rel_dir: String,
    /// 含 `repo:` 行的笔记相对路径（人写，只读）。
    pub note_rel: String,
    /// (owner, repo)。
    pub repo: (String, String),
}

#[derive(Debug, Serialize)]
pub struct ProjectsSyncReport {
    pub scanned: usize,
    pub github_projects: usize,
    /// README 或元数据发生覆写的项目名。
    pub updated: Vec<String>,
    pub errors: Vec<String>,
    pub committed: bool,
}

/// 项目库同步器：低频（默认每日）扫描 + GitHub 拉取 + Vault 覆写。
pub struct ProjectsTracker {
    config: ProjectsConfig,
    vault: Arc<dyn Vault>,
    github: GithubClient,
}

impl ProjectsTracker {
    pub fn new(config: ProjectsConfig, vault: Arc<dyn Vault>) -> Self {
        Self {
            config,
            vault,
            github: GithubClient::new(),
        }
    }

    pub async fn poll_once(&self) -> Result<ProjectsSyncReport> {
        let entries = self.scan_projects().unwrap_or_else(|error| {
            // 扫描失败（目录未建等）不算任务失败，退化为空集。
            eprintln!("[projects] scan failed: {error:#}");
            Vec::new()
        });
        let mut updated = Vec::new();
        let mut errors = Vec::new();
        let github_projects = entries.len();
        for entry in &entries {
            match self.sync_one(entry).await {
                Ok(true) => updated.push(entry.name.clone()),
                Ok(false) => {}
                Err(error) => errors.push(format!("{}: {error:#}", entry.name)),
            }
        }
        let committed = if updated.is_empty() {
            false
        } else {
            self.vault
                .commit_all("[ru41] projects GitHub 同步")?
        };
        if committed {
            self.vault.push()?;
        }
        Ok(ProjectsSyncReport {
            scanned: self.count_project_dirs().unwrap_or(0),
            github_projects,
            updated,
            errors,
            committed,
        })
    }

    /// 扫描项目库：projects_dir 下一层目录即项目（平铺结构，无状态分组；
    /// 归档用笔记 frontmatter `status: archived` 标记，不影响同步）。
    /// 目录不存在视为未启用（空集），不报错。
    fn scan_projects(&self) -> Result<Vec<ProjectEntry>> {
        let root = self.config.vault_root.join(&self.config.projects_dir);
        let mut entries = Vec::new();
        let Ok(projects) = std::fs::read_dir(&root) else {
            return Ok(entries);
        };
        for project in projects.flatten() {
            if !project.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = project.file_name().to_string_lossy().into_owned();
            let found = find_repo_note(&project.path(), &self.config.projects_dir, &name);
            let Some((note_rel, repo)) = found else {
                continue;
            };
            entries.push(ProjectEntry {
                rel_dir: format!("{}/{name}", self.config.projects_dir),
                name,
                note_rel,
                repo,
            });
        }
        entries.sort_by(|a, b| a.rel_dir.cmp(&b.rel_dir));
        Ok(entries)
    }

    fn count_project_dirs(&self) -> Result<usize> {
        let root = self.config.vault_root.join(&self.config.projects_dir);
        let Ok(projects) = std::fs::read_dir(&root) else {
            return Ok(0);
        };
        Ok(projects
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .count())
    }

    /// 同步单个项目：README + 元数据渲染后与 vault 现状比对，变化才覆写。
    /// 返回 true 表示发生了写入。
    async fn sync_one(&self, entry: &ProjectEntry) -> Result<bool> {
        let (owner, repo) = &entry.repo;
        let meta = self
            .github
            .repo_meta(owner, repo)
            .await
            .with_context(|| format!("github meta 拉取失败 {owner}/{repo}"))?;
        let readme = self
            .github
            .readme(owner, repo, &meta.default_branch)
            .await
            .unwrap_or_else(|error| {
                // 无 README 的仓库不判失败，元数据照样落。
                eprintln!("[projects] {} readme skipped: {error:#}", entry.name);
                String::new()
            });
        let rendered = render_repo_note(&entry.note_rel, owner, repo, &meta, &readme);
        let target = format!("{}/Repo-GitHub.md", entry.rel_dir);
        let unchanged = self
            .vault
            .read_file(&target)?
            .is_some_and(|current| current == rendered);
        if unchanged {
            return Ok(false);
        }
        self.vault.write_file(&target, &rendered)?;
        Ok(true)
    }
}

impl crate::ingress::schedule::PollJob for ProjectsTracker {
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

/// 在项目目录里找第一个 frontmatter 带 `repo: https://github.com/...` 的笔记。
/// 跳过机器拥有的 Repo-GitHub.md。
fn find_repo_note(
    project_dir: &Path,
    projects_dir: &str,
    name: &str,
) -> Option<(String, (String, String))> {
    let mut notes = std::fs::read_dir(project_dir).ok()?.flatten();
    while let Some(note) = notes.next() {
        let file_name = note.file_name().to_string_lossy().into_owned();
        if !file_name.ends_with(".md") || file_name == "Repo-GitHub.md" {
            continue;
        }
        let text = std::fs::read_to_string(note.path()).ok()?;
        let Some(repo_url) = frontmatter_repo(&text) else {
            continue;
        };
        let Some(repo) = GithubClient::parse_repo(&repo_url) else {
            continue;
        };
        return Some((
            format!("{projects_dir}/{name}/{file_name}"),
            repo,
        ));
    }
    None
}

/// 从笔记文本的 frontmatter 提取 `repo:` 值（容忍引号）；无 frontmatter 或无该行返回 None。
fn frontmatter_repo(text: &str) -> Option<String> {
    let body = text.strip_prefix("---")?;
    let end = body.find("\n---")?;
    for line in body[..end].lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("repo:") {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// Repo-GitHub.md 渲染：机器元数据 frontmatter + README 原文。
fn render_repo_note(
    note_rel: &str,
    owner: &str,
    repo: &str,
    meta: &GithubRepoMeta,
    readme: &str,
) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("project_note: {note_rel}\n"));
    out.push_str(&format!("repo: https://github.com/{owner}/{repo}\n"));
    if let Some(desc) = &meta.description {
        out.push_str(&format!("description: {}\n", desc.replace('\n', " ")));
    }
    out.push_str(&format!("stars: {}\n", meta.stargazers_count));
    out.push_str(&format!("open_issues: {}\n", meta.open_issues_count));
    out.push_str(&format!("pushed_at: {}\n", meta.pushed_at));
    out.push_str(&format!("archived: {}\n", meta.archived));
    out.push_str(&format!("default_branch: {}\n", meta.default_branch));
    out.push_str("---\n\n");
    out.push_str(
        "> 本文件由 Ru41 从 GitHub 自动同步（README + 仓库元数据），请勿手改；\n\
         > 项目目标、进度、你的思考写在本目录其他笔记。\n\n",
    );
    out.push_str(&format!("# {} · GitHub 快照\n\n", meta.full_name));
    out.push_str(readme.trim_end());
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::{find_repo_note, frontmatter_repo};

    #[test]
    fn frontmatter_repo_extracts_github_url() {
        let text = "---\nname: Ru41\nrepo: https://github.com/windy664/Ru41\nstatus: active\n---\n# 正文";
        assert_eq!(
            frontmatter_repo(text).as_deref(),
            Some("https://github.com/windy664/Ru41")
        );
        let quoted = "---\nrepo: \"https://github.com/o/r\"\n---\n";
        assert_eq!(
            frontmatter_repo(quoted).as_deref(),
            Some("https://github.com/o/r")
        );
        assert!(frontmatter_repo("# 没有 frontmatter").is_none());
        assert!(frontmatter_repo("---\nname: x\n---\n").is_none());
    }

    #[test]
    fn repo_note_skips_machine_file_and_non_github() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Repo-GitHub.md"), "---\nrepo: https://github.com/o/r\n---\n").unwrap();
        // 机器文件被跳过 → None。
        assert!(find_repo_note(dir.path(), "P", "n").is_none());
        std::fs::write(dir.path().join("笔记.md"), "---\nrepo: https://gitee.com/a/b\n---\n").unwrap();
        // 非 GitHub 链接不算 GitHub 项目。
        assert!(find_repo_note(dir.path(), "P", "n").is_none());
        std::fs::write(
            dir.path().join("项目笔记.md"),
            "---\nrepo: https://github.com/o/r\n---\n# Ru41\n",
        )
        .unwrap();
        let (rel, repo) = find_repo_note(dir.path(), "P", "n").unwrap();
        assert_eq!(rel, "P/n/项目笔记.md");
        assert_eq!(repo, ("o".to_string(), "r".to_string()));
    }
}
