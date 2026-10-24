use anyhow::{Context, Result};

/// GitHub 仓库元数据（api.github.com/repos/{owner}/{repo} 的子集）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GithubRepoMeta {
    pub full_name: String,
    pub description: Option<String>,
    pub stargazers_count: u64,
    pub open_issues_count: u64,
    pub pushed_at: String,
    pub default_branch: String,
    pub archived: bool,
}

/// GitHub 公共客户端：免鉴权（60 次/时/IP），公开仓库的 README 与元数据抓取。
/// 速率预算由调用方控制——projects 同步为低频任务，项目数量级远低于上限。
pub struct GithubClient {
    http: reqwest::Client,
}

impl Default for GithubClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GithubClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    /// 解析 GitHub 仓库地址为 (owner, repo)；非 github.com 链接返回 None。
    pub fn parse_repo(url: &str) -> Option<(String, String)> {
        let rest = url
            .trim()
            .trim_start_matches("https://github.com/")
            .trim_start_matches("http://github.com/")
            .trim_start_matches("github.com/");
        let mut parts = rest.split('/');
        let owner = parts.next()?.trim().to_owned();
        let repo = parts
            .next()?
            .trim()
            .trim_end_matches(".git")
            .trim_end_matches('/')
            .to_owned();
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        Some((owner, repo))
    }

    pub async fn repo_meta(&self, owner: &str, repo: &str) -> Result<GithubRepoMeta> {
        let url = format!("https://api.github.com/repos/{owner}/{repo}");
        self.http
            .get(&url)
            .send()
            .await
            .context("github api request failed")?
            .error_for_status()
            .context("github api returned error status")?
            .json()
            .await
            .context("github api json decode failed")
    }

    /// 拉取仓库 README 原文（raw 直链，先默认分支再 main/master 兜底）。
    pub async fn readme(&self, owner: &str, repo: &str, default_branch: &str) -> Result<String> {
        for branch in [default_branch, "main", "master"] {
            let url = format!("https://raw.githubusercontent.com/{owner}/{repo}/{branch}/README.md");
            let response = self
                .http
                .get(&url)
                .send()
                .await
                .with_context(|| format!("readme request failed: {url}"))?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                continue;
            }
            return response
                .error_for_status()
                .with_context(|| format!("readme returned error status: {url}"))?
                .text()
                .await
                .context("readme body read failed");
        }
        anyhow::bail!("README.md not found on default branch/main/master")
    }
}

#[cfg(test)]
mod tests {
    use super::GithubClient;

    #[test]
    fn parses_github_urls() {
        assert_eq!(
            GithubClient::parse_repo("https://github.com/windy664/Ru41"),
            Some(("windy664".into(), "Ru41".into()))
        );
        assert_eq!(
            GithubClient::parse_repo("https://github.com/o/r.git/"),
            Some(("o".into(), "r".into()))
        );
        assert!(GithubClient::parse_repo("https://gitee.com/a/b").is_none());
        assert!(GithubClient::parse_repo("https://github.com/onlyowner").is_none());
    }
}
