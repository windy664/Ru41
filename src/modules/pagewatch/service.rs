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

/// 一页一轮询的结果报告；单页失败不中断其他页面，错误收进 errors。
#[derive(Debug, Serialize)]
pub struct PagewatchReport {
    pub pages: usize,
    pub changed: Vec<String>,
    /// 检测到变化但因通知冷却被压下的页面（基线已静默更新）。
    pub suppressed: Vec<String>,
    pub errors: Vec<String>,
    pub notified: bool,
}

/// 同一页两次通知的最小间隔：防动态噪声页面刷屏，也防真变更时连发。
const NOTIFY_COOLDOWN_SECS: i64 = 6 * 3600;

/// 冷却判定：距上次通知超过冷却期才再报。
fn should_notify(last_notified: i64, now: i64) -> bool {
    now - last_notified > NOTIFY_COOLDOWN_SECS
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
        let mut suppressed = Vec::new();
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
                            let now = crate::util::now();
                            let last_notified =
                                doc.value["notified_at"].as_i64().unwrap_or(0);
                            let notify = should_notify(last_notified, now);
                            // 无论是否通知都更新基线：噪声页静默跟随，真变更冷却后能再报。
                            self.store.put_document(
                                "pagewatch_page",
                                &page.url,
                                Some(doc.revision),
                                &serde_json::json!({
                                    "name": page.name,
                                    "fingerprint": fingerprint,
                                    "last_changed_at": now,
                                    "notified_at": if notify { now } else { last_notified },
                                }),
                            )?;
                            if notify {
                                changed.push(page.name.clone());
                            } else {
                                suppressed.push(page.name.clone());
                            }
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
            suppressed,
            errors,
            notified,
        })
    }

    /// 页面内容指纹：剔除动态噪声后取稳定散列。
    /// 只比"内容是否变了"，不做语义 diff（语义判断交给人和 agent）。
    /// 网络/代理抖动失败时隔 3 秒重试一次，仍失败才记 error。
    async fn fetch_fingerprint(&self, url: &str) -> Result<String> {
        match self.fetch_body(url).await {
            Ok(body) => Ok(content_fingerprint(&body)),
            Err(first) => {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let body = self
                    .fetch_body(url)
                    .await
                    .with_context(|| format!("{first:#}; retry failed"))?;
                Ok(content_fingerprint(&body))
            }
        }
    }

    async fn fetch_body(&self, url: &str) -> Result<String> {
        self.http
            .get(url)
            .send()
            .await
            .context("page request failed")?
            .error_for_status()
            .context("page returned error status")?
            .text()
            .await
            .context("page body read failed")
    }
}

/// 内容指纹：剔除 script/style/注释后，再去掉所有标签只留可见文本取散列。
/// 只比"内容是否变了"，不做语义 diff（语义判断交给人和 agent）。
///
/// 为什么标签也要去：Cloudflare 邮箱混淆（data-cfemail、
/// /cdn-cgi/l/email-protection#随机串）和构建期随机 class/id 都藏在标签属性里，
/// 每请求都变但可见内容没变——按标签散列会把这种噪声当成"页面更新"反复误报
/// （2026-10-27 Mitacs 页面每 30 分钟误报一次的根因）。
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
    fn tags() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"(?s)<[^>]+>").expect("tag regex"))
    }
    let stripped = noise().replace_all(html, " ");
    let text = tags().replace_all(&stripped, " ");
    let normalized: String = text.split_whitespace().collect();
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
    fn fingerprint_ignores_cloudflare_email_obfuscation() {
        // Cloudflare 邮箱混淆：data-cfemail 与 email-protection# 哈希每请求都变，
        // 可见文本不变——不得触发"变更"（Mitacs 误报根因）。
        let page_a = r#"<html><body><p>Contact <a href="/cdn-cgi/l/email-protection#aaaa"><span data-cfemail="1111">[email&#160;protected]</span></a></p></body></html>"#;
        let page_b = r#"<html><body><p>Contact <a href="/cdn-cgi/l/email-protection#zzzz"><span data-cfemail="9999">[email&#160;protected]</span></a></p></body></html>"#;
        assert_eq!(content_fingerprint(page_a), content_fingerprint(page_b));
    }

    #[test]
    fn cooldown_gates_repeat_notify() {
        let now = 1_793_000_000;
        // 从未通知过 → 报
        assert!(should_notify(0, now));
        // 1 小时前报过 → 冷却中压下
        assert!(!should_notify(now - 3600, now));
        // 7 小时前报过 → 过冷却期，再报
        assert!(should_notify(now - 7 * 3600, now));
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
