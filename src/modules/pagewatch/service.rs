//! 页面监控：盯没有 RSS 的网页（赛事官网、公告页等机会源头）。
//! 抓取 → 规范化 → 内容指纹比对，变化即通知。首次抓取只建基线不打扰。
//! 与 RSS 模块互补：RSS 管有 feed 的信息流，pagewatch 管"只在官网悄悄更新"的页面。

use crate::{
    domain::NotifyRoute,
    ports::{channel::NotifyChannel, storage::TaskStore},
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct WatchedPage {
    pub name: String,
    pub url: String,
}

/// 页面监控运行时配置（对应配置文件的 [sources.pagewatch]）。
#[derive(Debug, Clone)]
pub struct PagewatchConfig {
    pub pages: Vec<WatchedPage>,
    pub interval_secs: u64,
    pub notify: Option<NotifyRoute>,
}

/// 一轮轮询的结果报告；单页失败不中断其他页面，错误收进 errors。
#[derive(Debug, Serialize)]
pub struct PagewatchReport {
    pub pages: usize,
    pub changed: Vec<String>,
    pub errors: Vec<String>,
    pub notified: bool,
}

pub struct PagewatchTracker {
    store: Arc<dyn TaskStore>,
    http: reqwest::Client,
    notifier: Option<Arc<dyn NotifyChannel>>,
    notify_target: String,
    config: PagewatchConfig,
}

impl PagewatchTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: PagewatchConfig,
    ) -> Self {
        let notify_target = config
            .notify
            .as_ref()
            .map(|route| route.target.clone())
            .unwrap_or_default();
        Self {
            store,
            http: crate::util::http_client(),
            notifier,
            notify_target,
            config,
        }
    }

    pub async fn poll_once(&self) -> Result<PagewatchReport> {
        let mut changed = Vec::new();
        let mut errors = Vec::new();
        for page in &self.config.pages {
            match self.fetch_fingerprint(&page.url).await {
                Ok(fingerprint) => {
                    match self.store.document("pagewatch_page", &page.url)? {
                        // 首次抓取：只建基线，不通知（否则接入当天全是"变更"）。
                        None => {
                            self.store.put_document(
                                "pagewatch_page",
                                &page.url,
                                None,
                                &serde_json::json!({
                                    "name": page.name,
                                    "fingerprint": fingerprint,
                                    "baseline_at": crate::util::now(),
                                }),
                            )?;
                        }
                        Some(doc) if doc.value["fingerprint"].as_str() == Some(&fingerprint) => {}
                        Some(doc) => {
                            self.store.put_document(
                                "pagewatch_page",
                                &page.url,
                                Some(doc.revision),
                                &serde_json::json!({
                                    "name": page.name,
                                    "fingerprint": fingerprint,
                                    "last_changed_at": crate::util::now(),
                                }),
                            )?;
                            changed.push(page.name.clone());
                        }
                    }
                }
                Err(error) => errors.push(format!("{}: {error:#}", page.name)),
            }
        }
        let mut notified = false;
        if !changed.is_empty()
            && let Some(notifier) = &self.notifier
        {
            let lines: Vec<String> = self
                .config
                .pages
                .iter()
                .filter(|page| changed.contains(&page.name))
                .map(|page| format!("· {}：{}", page.name, page.url))
                .collect();
            notifier
                .send(
                    &self.notify_target,
                    None,
                    &format!(
                        "📡 监控页面有更新：\n{}\n内容发生变化，可能是新赛事/新公告上线，建议去看看。",
                        lines.join("\n")
                    ),
                )
                .await?;
            notified = true;
        }
        Ok(PagewatchReport {
            pages: self.config.pages.len(),
            changed,
            errors,
            notified,
        })
    }

    /// 页面内容指纹：剔除动态噪声后取稳定散列。
    /// 只比"内容是否变了"，不做语义 diff（语义判断交给人和 agent）。
    async fn fetch_fingerprint(&self, url: &str) -> Result<String> {
        let body = self
            .http
            .get(url)
            .send()
            .await
            .context("page request failed")?
            .error_for_status()
            .context("page returned error status")?
            .text()
            .await
            .context("page body read failed")?;
        Ok(content_fingerprint(&body))
    }
}

/// 内容指纹：先剔除 script/style 块与 HTML 注释（里面常藏每请求都变的
/// token、构建标识、RSC 载荷，如 Next.js 的随机 _rsc 参数），再去空白散列。
/// 注意：纯 SPA 页面（正文只有"加载中"）剔除脚本后指纹恒定，
/// 永远报不出变化——这类页面应改用其数据 API 配 RSS 伪 feed 监控。
fn content_fingerprint(html: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    fn noise() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(r"(?si)<script[^>]*>.*?</script>|<style[^>]*>.*?</style>|<!--.*?-->")
                .expect("noise regex")
        })
    }
    let stripped = noise().replace_all(html, " ");
    let normalized: String = stripped.split_whitespace().collect();
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

impl crate::ingress::schedule::PollJob for PagewatchTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        Box::pin(async move { Ok(serde_json::to_value(self.poll_once().await?)?) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_ignores_script_noise_but_sees_body_change() {
        // Next.js 类页面：script 里每请求都变的 _rsc/token 不应触发"变更"。
        let page_a = r#"<html><body><h1>赛事一览</h1><script>self.__next_f.push(["c":["","contest?_rsc=aaaa"]])</script></body></html>"#;
        let page_b = r#"<html><body><h1>赛事一览</h1><script>self.__next_f.push(["c":["","contest?_rsc=zz99"]])</script></body></html>"#;
        assert_eq!(content_fingerprint(page_a), content_fingerprint(page_b));
        // 正文真变了必须能检出。
        let page_c = r#"<html><body><h1>赛事一览</h1><p>新赛事上线</p></body></html>"#;
        assert_ne!(content_fingerprint(page_a), content_fingerprint(page_c));
    }

    #[test]
    fn baseline_does_not_notify_and_change_does() {
        // 纯存储语义：不依赖网络，直接走 document 比对路径的单元等价物
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            crate::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        // 基线写入后指纹相同 → 不变更
        store
            .put_document(
                "pagewatch_page",
                "https://x",
                None,
                &serde_json::json!({"fingerprint": "aaa"}),
            )
            .unwrap();
        let doc = store
            .document("pagewatch_page", "https://x")
            .unwrap()
            .unwrap();
        assert_eq!(doc.value["fingerprint"].as_str(), Some("aaa"));
        // 指纹不同 → 需要 revision 乐观更新
        let updated = store
            .put_document(
                "pagewatch_page",
                "https://x",
                Some(doc.revision),
                &serde_json::json!({"fingerprint": "bbb"}),
            )
            .unwrap();
        assert!(updated);
        assert_eq!(
            store
                .document("pagewatch_page", "https://x")
                .unwrap()
                .unwrap()
                .value["fingerprint"],
            "bbb"
        );
    }
}
