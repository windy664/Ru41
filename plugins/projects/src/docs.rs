//! 仓库文档同步：把 GitHub 仓库里的 markdown 文档抓进 vault 的项目目录。
//!
//! 为什么不用 GitHub API 列举文件：这个账号有 100+ 个仓库，匿名 API 只有
//! 60 次/时，光 README 的仓库元数据就吃光了（实测长期处于 rate limit 干旱，
//! README 之外的文档根本轮不到）。jsDelivr 的文件清单接口不占 GitHub 配额，
//! 一次请求给出整棵目录树，够用了。
//!
//! 抓哪部分：仓库根目录的 markdown + `doc/`、`docs/` 目录下的 markdown。
//! README 单独同步（它是项目快照的正文），这里不重复。

use anyhow::{Context, Result};
use serde::Deserialize;

/// 项目目录下机器写文档的子目录（人写的笔记不落这里）。
pub const DOCS_DIR: &str = "GitHub-Docs";
/// 单个仓库最多同步的文档数。
pub const MAX_DOCS_PER_REPO: usize = 15;
/// 单文档最大字节数（超了跳过，避免把大文件塞进 vault）。
pub const MAX_DOC_BYTES: u64 = 64 * 1024;
/// 文档来源目录：仓库根目录 + 这些目录下的 markdown。
pub const DOC_SOURCE_DIRS: &[&str] = &["doc", "docs"];

/// jsDelivr 的文件清单响应（structure=flat 时是扁平列表）。
#[derive(Debug, Default, Deserialize)]
pub struct FileListing {
    #[serde(default)]
    pub files: Vec<ListedFile>,
}

#[derive(Debug, Deserialize)]
pub struct ListedFile {
    /// 形如 `/doc/guide/intro.md`。
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

/// 从清单里挑出要同步的 markdown（仓库内相对路径，不带前导 /）。
///
/// 只取仓库根目录与 `doc/`、`docs/` 下的：再往下的目录多半是源码树里的
/// 零散说明，抓进来只会把 vault 撑成第二个代码仓库。
pub fn select_markdown(listing: &FileListing) -> Vec<String> {
    let mut picked: Vec<String> = Vec::new();
    for file in &listing.files {
        let path = file.name.trim_start_matches('/');
        if !path.to_ascii_lowercase().ends_with(".md") {
            continue;
        }
        let in_root = !path.contains('/');
        let in_source_dir = DOC_SOURCE_DIRS
            .iter()
            .any(|dir| path.starts_with(&format!("{dir}/")));
        if !in_root && !in_source_dir {
            continue;
        }
        // README 已经作为项目快照的正文同步过了。
        if path.eq_ignore_ascii_case("readme.md") {
            continue;
        }
        if file.size > MAX_DOC_BYTES {
            continue;
        }
        picked.push(path.to_owned());
        if picked.len() >= MAX_DOCS_PER_REPO {
            break;
        }
    }
    picked.sort();
    picked
}

/// 文档抓取器（jsDelivr 列目录 + raw 直链取正文）。
#[derive(Clone)]
pub struct DocsClient {
    http: reqwest::Client,
}

impl Default for DocsClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DocsClient {
    pub fn new() -> Self {
        Self {
            http: ru41::util::http_client(),
        }
    }

    /// 列出仓库里要同步的 markdown 相对路径，并给出**实际用到的分支**。
    ///
    /// 分支名对不上（仓库改了默认分支）就换另一个常见分支再试；返回的分支
    /// 必须跟着走——拿 A 分支的清单去 B 分支取正文会 404。
    pub async fn list_markdown(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<(String, Vec<String>)> {
        let mut branches = vec![branch.to_owned()];
        for fallback in ["master", "main"] {
            if !branches.iter().any(|b| b == fallback) {
                branches.push(fallback.to_owned());
            }
        }
        let mut last_err = None;
        for candidate in branches {
            match self.fetch_listing(owner, repo, &candidate).await {
                Ok(listing) => return Ok((candidate, select_markdown(&listing))),
                Err(error) => last_err = Some(error),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("没有可用的分支")))
    }

    async fn fetch_listing(&self, owner: &str, repo: &str, branch: &str) -> Result<FileListing> {
        let url = format!(
            "https://data.jsdelivr.com/v1/packages/gh/{owner}/{repo}@{branch}?structure=flat"
        );
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("文件清单请求失败: {url}"))?
            .error_for_status()
            .with_context(|| format!("文件清单返回错误状态: {url}"))?;
        response.json().await.context("文件清单解析失败")
    }

    /// 取文档原文。
    pub async fn fetch(&self, owner: &str, repo: &str, branch: &str, path: &str) -> Result<String> {
        let url = format!("https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{path}");
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("文档请求失败: {url}"))?
            .error_for_status()
            .with_context(|| format!("文档返回错误状态: {url}"))?;
        let text = response.text().await.context("文档正文读取失败")?;
        anyhow::ensure!(
            text.len() as u64 <= MAX_DOC_BYTES,
            "文档超过大小上限: {path}"
        );
        Ok(text)
    }
}

/// 机器同步文档的头部（让人一眼看出这是自动抓的）。
pub fn render_doc(repo: &str, branch: &str, path: &str, body: &str) -> String {
    format!(
        "<!-- 由 Ru41 从 GitHub 仓库同步（{repo}@{branch}/{path}），请勿手改；\
         要写自己的笔记请在本目录另开文件。 -->\n\n{body}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(files: &[(&str, u64)]) -> FileListing {
        FileListing {
            files: files
                .iter()
                .map(|(name, size)| ListedFile {
                    name: (*name).to_owned(),
                    size: *size,
                })
                .collect(),
        }
    }

    /// 只取根目录与 doc/ 下的 markdown，README 与源码树里的说明不抓。
    #[test]
    fn selects_root_and_doc_markdown_only() {
        let picked = select_markdown(&listing(&[
            ("/README.md", 10),
            ("/CHANGELOG.md", 10),
            ("/doc/guide.md", 10),
            ("/doc/deep/intro.md", 10),
            ("/docs/plan.md", 10),
            ("/src/main.rs", 10),
            ("/src/notes.md", 10),
            ("/other/readme.md", 10),
        ]));
        assert_eq!(
            picked,
            vec![
                "CHANGELOG.md".to_owned(),
                "doc/deep/intro.md".to_owned(),
                "doc/guide.md".to_owned(),
                "docs/plan.md".to_owned(),
            ]
        );
    }

    /// 超大文件不抓：vault 是笔记库，不是第二个代码仓库。
    #[test]
    fn skips_oversized_files() {
        let picked = select_markdown(&listing(&[
            ("/big.md", MAX_DOC_BYTES + 1),
            ("/small.md", 12),
        ]));
        assert_eq!(picked, vec!["small.md".to_owned()]);
    }
}
