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
            let url =
                format!("https://raw.githubusercontent.com/{owner}/{repo}/{branch}/README.md");
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

    /// 列出账号全部公开仓库。api.github.com 优先（一次调用带全部元数据）；
    /// 限流时退回 HTML 列表页（只有名字，pushed_at 等由 repo_meta 补）。
    pub async fn list_user_repos(&self, user: &str) -> Result<Vec<GithubRepoMeta>> {
        let url = format!("https://api.github.com/users/{user}/repos?per_page=100&sort=updated");
        match self.http.get(&url).send().await {
            Ok(response) if response.status().is_success() => {
                return response
                    .json()
                    .await
                    .context("github repos json decode failed");
            }
            Ok(response) => {
                eprintln!("[github] repos api {}，退回 HTML 抓取", response.status());
            }
            Err(error) => eprintln!("[github] repos api 失败，退回 HTML 抓取: {error:#}"),
        }
        let names = self.list_repo_names_html(user).await?;
        let mut metas = Vec::new();
        for name in names {
            match self.repo_meta(user, &name).await {
                Ok(meta) => metas.push(meta),
                Err(error) => {
                    eprintln!("[github] {user}/{name} meta 失败，用 HTML 兜底: {error:#}");
                    metas.push(self.repo_meta_html(user, &name).await?);
                }
            }
        }
        Ok(metas)
    }

    /// HTML 抓取账号仓库名列表（repositories 标签页）。
    async fn list_repo_names_html(&self, user: &str) -> Result<Vec<String>> {
        let url = format!("https://github.com/{user}?tab=repositories");
        let html = self.get_html(&url).await?;
        let mut names = Vec::new();
        let prefix = format!("href=\"/{user}/");
        for chunk in html.split(&prefix).skip(1) {
            let name: String = chunk
                .chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
                .collect();
            if !name.is_empty()
                && !matches!(
                    name.as_str(),
                    "stargazers"
                        | "followers"
                        | "following"
                        | "repositories"
                        | "projects"
                        | "packages"
                        | "sponsors"
                        | "watching"
                )
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    /// HTML 抓取单仓库元数据：embedded JSON 里的 pushedAt / defaultBranch /
    /// isArchived；description 从 og:description 取。API 限流时的兜底。
    async fn repo_meta_html(&self, owner: &str, repo: &str) -> Result<GithubRepoMeta> {
        let url = format!("https://github.com/{owner}/{repo}");
        let html = self.get_html(&url).await?;
        let pick = |key: &str| -> Option<String> {
            let marker = format!("\"{key}\":\"");
            let start = html.find(&marker)? + marker.len();
            let end = html[start..].find('"')? + start;
            Some(html[start..end].to_owned())
        };
        Ok(GithubRepoMeta {
            full_name: format!("{owner}/{repo}"),
            description: pick("description"),
            stargazers_count: 0,
            open_issues_count: 0,
            pushed_at: pick("pushedAt").unwrap_or_default(),
            default_branch: pick("defaultBranch").unwrap_or_else(|| "main".into()),
            archived: html.contains("\"isArchived\":true"),
        })
    }

    async fn get_html(&self, url: &str) -> Result<String> {
        self.http
            .get(url)
            .header(reqwest::header::USER_AGENT, "Mozilla/5.0")
            .send()
            .await
            .with_context(|| format!("github html request failed: {url}"))?
            .error_for_status()
            .with_context(|| format!("github html returned error: {url}"))?
            .text()
            .await
            .context("github html body read failed")
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
