//! 岗位 JD 补全：面灵列表不带 JD 正文（详情接口要登录），
//! 按平台回源到官方招聘站的公开 API 抓 JD——
//! ashby / greenhouse / smartrecruiters 三家免登录、结构化、带全文。
//! 国内平台（feishu-hire / meituan / tencent / jd / workday）接口要登录或未调研，暂缺。
//! 匹配方式：公司名猜 board slug + 岗位标题归一化匹配；抓不到返回 None（调用方记状态，不反复抓）。

use anyhow::Result;

/// JD 命中：纯文本 + 官方岗位页链接（链接用于总览可点击跳转）。
pub struct JdHit {
    pub text: String,
    pub url: Option<String>,
}

/// 抓某岗位的 JD 纯文本（截断到 ~1500 字符）。不支持的平台/匹配失败返回 None。
pub async fn fetch_jd(
    http: &reqwest::Client,
    platform: &str,
    company: &str,
    title: &str,
) -> Option<JdHit> {
    let slug = guess_slug(company);
    let result = match platform {
        "ashby" => ashby(http, &slug, title).await,
        "greenhouse" => greenhouse(http, &slug, title).await,
        "smartrecruiters" => smartrecruiters(http, &slug, title).await,
        _ => return None,
    };
    match result {
        Ok(text) => text,
        Err(error) => {
            eprintln!("[jobs.jd] {platform}/{slug} fetch failed: {error:#}");
            None
        }
    }
}

/// 公司名 → board slug 猜测：小写、去非字母数字（OpenAI→openai，Flow Traders→flowtraders）。
fn guess_slug(company: &str) -> String {
    company
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// 标题归一化：小写，只留字母数字与中日韩字符。
fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// 标题匹配：归一化后相等或互相包含（处理 "Software Engineer" vs "Software Engineer, Monetization"）。
fn title_matches(a: &str, b: &str) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    !a.is_empty() && !b.is_empty() && (a == b || a.contains(&b) || b.contains(&a))
}

/// HTML → 纯文本：去标签、压空白、截断。
fn html_to_text(html: &str) -> String {
    let tag_re = regex::Regex::new(r"(?s)<[^>]+>").expect("tag regex");
    let text = tag_re.replace_all(html, " ");
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 1500;
    if text.chars().count() > MAX {
        text.chars().take(MAX).collect::<String>() + "…"
    } else {
        text
    }
}

async fn get_json<T: serde::de::DeserializeOwned>(http: &reqwest::Client, url: &str) -> Result<T> {
    Ok(http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// https://api.ashbyhq.com/posting-api/job-board/{slug}（全量 board，含 descriptionHtml）。
async fn ashby(http: &reqwest::Client, slug: &str, title: &str) -> Result<Option<JdHit>> {
    #[derive(serde::Deserialize)]
    struct Board {
        jobs: Vec<Job>,
    }
    #[derive(serde::Deserialize)]
    struct Job {
        id: String,
        title: String,
        #[serde(rename = "descriptionHtml")]
        description_html: String,
        #[serde(rename = "jobUrl", default)]
        job_url: Option<String>,
    }
    let board: Board = get_json(
        http,
        &format!("https://api.ashbyhq.com/posting-api/job-board/{slug}"),
    )
    .await?;
    Ok(board
        .jobs
        .into_iter()
        .find(|job| title_matches(&job.title, title))
        .map(|job| JdHit {
            text: html_to_text(&job.description_html),
            // 官方岗位页：优先 API 自带 jobUrl，缺省按 id 构造。
            url: job
                .job_url
                .filter(|u| !u.is_empty())
                .or(Some(format!("https://jobs.ashbyhq.com/{slug}/{}", job.id))),
        }))
}

/// https://boards-api.greenhouse.io/v1/boards/{slug}/jobs → 匹配 id → /jobs/{id} 取 content。
async fn greenhouse(http: &reqwest::Client, slug: &str, title: &str) -> Result<Option<JdHit>> {
    #[derive(serde::Deserialize)]
    struct List {
        jobs: Vec<ListJob>,
    }
    #[derive(serde::Deserialize)]
    struct ListJob {
        id: u64,
        title: String,
    }
    #[derive(serde::Deserialize)]
    struct Detail {
        content: String,
    }
    let list: List = get_json(
        http,
        &format!("https://boards-api.greenhouse.io/v1/boards/{slug}/jobs"),
    )
    .await?;
    let Some(job) = list
        .jobs
        .into_iter()
        .find(|j| title_matches(&j.title, title))
    else {
        return Ok(None);
    };
    let detail: Detail = get_json(
        http,
        &format!(
            "https://boards-api.greenhouse.io/v1/boards/{slug}/jobs/{}",
            job.id
        ),
    )
    .await?;
    Ok(Some(JdHit {
        text: html_to_text(&detail.content),
        url: Some(format!(
            "https://boards.greenhouse.io/{slug}/jobs/{}",
            job.id
        )),
    }))
}

/// https://api.smartrecruiters.com/v1/companies/{slug}/postings → 匹配 id → /postings/{id}。
async fn smartrecruiters(http: &reqwest::Client, slug: &str, title: &str) -> Result<Option<JdHit>> {
    #[derive(serde::Deserialize)]
    struct List {
        content: Vec<ListItem>,
    }
    #[derive(serde::Deserialize)]
    struct ListItem {
        id: String,
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct Detail {
        sections: Option<Sections>,
    }
    #[derive(serde::Deserialize)]
    struct Sections {
        #[serde(rename = "jobDescription")]
        job_description: Option<JobDesc>,
    }
    #[derive(serde::Deserialize)]
    struct JobDesc {
        sections: Option<Vec<Section>>,
    }
    #[derive(serde::Deserialize)]
    struct Section {
        text: String,
    }
    let list: List = get_json(
        http,
        &format!("https://api.smartrecruiters.com/v1/companies/{slug}/postings?limit=100"),
    )
    .await?;
    let Some(item) = list
        .content
        .into_iter()
        .find(|i| title_matches(&i.name, title))
    else {
        return Ok(None);
    };
    let item_id = item.id.clone();
    let detail: Detail = get_json(
        http,
        &format!(
            "https://api.smartrecruiters.com/v1/companies/{slug}/postings/{}",
            item.id
        ),
    )
    .await?;
    let text = detail
        .sections
        .and_then(|s| s.job_description)
        .and_then(|jd| jd.sections)
        .map(|sections| {
            html_to_text(
                &sections
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        });
    Ok(text.filter(|t| !t.is_empty()).map(|text| JdHit {
        text,
        url: Some(format!(
            "https://jobs.smartrecruiters.com/{slug}/{}",
            item_id
        )),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_and_title_matching() {
        assert_eq!(guess_slug("OpenAI"), "openai");
        assert_eq!(guess_slug("Flow Traders"), "flowtraders");
        assert!(title_matches(
            "Software Engineer",
            "Software Engineer, Monetization Platform"
        ));
        assert!(title_matches(
            "后端开发实习生（IoT）",
            "后端开发实习生（IoT）"
        ));
        assert!(!title_matches("算法实习生", "产品经理"));
    }

    #[test]
    fn html_strip_and_truncate() {
        assert_eq!(html_to_text("<p>Hello <b>world</b></p>"), "Hello world");
        let long = format!("<p>{}</p>", "x".repeat(5000));
        assert!(html_to_text(&long).ends_with('…'));
    }
}
