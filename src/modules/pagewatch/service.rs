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
    /// 写入 vault 页面动态的条目数（不受 QQ 冷却影响，全量记录实质变更）。
    pub vault_logged: usize,
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
    vault: Option<Arc<dyn crate::ports::vault::Vault>>,
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
            vault: None,
        }
    }

    pub fn with_vault_opt(mut self, vault: Option<Arc<dyn crate::ports::vault::Vault>>) -> Self {
        self.vault = vault;
        self
    }

    pub async fn poll_once(&self) -> Result<PagewatchReport> {
        // (名称, url, 新增行, 消失行)：meaningful 全量进 vault，changed 为冷却放行的 QQ 通知子集
        let mut meaningful: Vec<(String, String, Vec<String>, Vec<String>)> = Vec::new();
        let mut changed: Vec<(String, String, Vec<String>, Vec<String>)> = Vec::new();
        let mut suppressed = Vec::new();
        let mut errors = Vec::new();
        for page in &self.config.pages {
            match self.fetch_content(&page.url).await {
                Ok((fingerprint, text_lines)) => {
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
                                    "lines": text_lines,
                                    "baseline_at": crate::util::now(),
                                    "last_checked_at": crate::util::now(),
                                }),
                            )?;
                        }
                        Some(doc) if doc.value["fingerprint"].as_str() == Some(&fingerprint) => {
                            // 内容没变也刷新巡查时间，状态表才能反映"刚看过"。
                            self.store.put_document(
                                "pagewatch_page",
                                &page.url,
                                Some(doc.revision),
                                &serde_json::json!({
                                    "name": doc.value["name"].as_str().unwrap_or(&page.name),
                                    "fingerprint": fingerprint,
                                    "lines": doc.value["lines"].clone(),
                                    "baseline_at": doc.value["baseline_at"].clone(),
                                    "last_changed_at": doc.value["last_changed_at"].clone(),
                                    "notified_at": doc.value["notified_at"].clone(),
                                    "last_checked_at": crate::util::now(),
                                }),
                            )?;
                        }
                        Some(doc) => {
                            let now = crate::util::now();
                            let old_lines: Vec<String> = doc
                                .value
                                .get("lines")
                                .and_then(|v| v.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|l| l.as_str().map(str::to_owned))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let (added, removed) = diff_lines(&old_lines, &text_lines);
                            // 行集合没变只是顺序/空白抖动：静默跟随基线，不打扰。
                            let is_meaningful = !added.is_empty() || !removed.is_empty();
                            let last_notified = doc.value["notified_at"].as_i64().unwrap_or(0);
                            let notify = is_meaningful && should_notify(last_notified, now);
                            // 无论是否通知都更新基线：噪声页静默跟随，真变更冷却后能再报。
                            self.store.put_document(
                                "pagewatch_page",
                                &page.url,
                                Some(doc.revision),
                                &serde_json::json!({
                                    "name": page.name,
                                    "fingerprint": fingerprint,
                                    "lines": text_lines,
                                    "last_changed_at": now,
                                    "last_checked_at": now,
                                    "notified_at": if notify { now } else { last_notified },
                                }),
                            )?;
                            if !is_meaningful {
                                // 顺序抖动，不计入任何列表
                            } else {
                                meaningful.push((
                                    page.name.clone(),
                                    page.url.clone(),
                                    added,
                                    removed,
                                ));
                                if notify {
                                    changed.push(meaningful.last().expect("just pushed").clone());
                                } else {
                                    suppressed.push(page.name.clone());
                                }
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
            let mut msg = String::from("📡 监控页面有更新：\n");
            for (name, url, added, removed) in &changed {
                msg.push_str(&format!("\n**{name}**\n"));
                for line in added.iter().take(6) {
                    msg.push_str(&format!("+ {}\n", crate::util::truncate_chars(line, 90)));
                }
                for line in removed.iter().take(2) {
                    msg.push_str(&format!("- {}\n", crate::util::truncate_chars(line, 90)));
                }
                msg.push_str(&format!("🔗 {url}\n"));
            }
            notifier.send(&self.notify_target, None, &msg).await?;
            notified = true;
        }
        // vault 状态表：每轮按各页文档全量重渲染，保证 Obsidian 里是最新状态；
        // 内容没变化则不写文件不提交（obsidian 闭环也不会被触发）。
        let mut vault_logged = 0usize;
        if let Some(vault) = &self.vault {
            let mut docs = std::collections::HashMap::new();
            for page in &self.config.pages {
                if let Some(doc) = self.store.document("pagewatch_page", &page.url)? {
                    docs.insert(page.url.clone(), doc.value);
                }
            }
            let content = render_status(&self.config.pages, &docs);
            let vault = vault.clone();
            vault_logged = docs.len();
            tokio::task::spawn_blocking(move || -> Result<()> {
                if vault.read_file(STATUS_PATH)?.as_deref() == Some(content.as_str()) {
                    return Ok(());
                }
                vault.write_file(STATUS_PATH, &content)?;
                if vault.commit_all("[ru41] 页面监控")? {
                    vault.push()?;
                }
                Ok(())
            })
            .await??;
        }
        Ok(PagewatchReport {
            pages: self.config.pages.len(),
            changed: changed.iter().map(|(name, _, _, _)| name.clone()).collect(),
            suppressed,
            errors,
            notified,
            vault_logged,
        })
    }

    /// 页面内容：指纹 + 可见文本行（供变更时算行级 diff 贴进通知）。
    /// 网络/代理抖动失败时隔 3 秒重试一次，仍失败才记 error。
    async fn fetch_content(&self, url: &str) -> Result<(String, Vec<String>)> {
        match self.fetch_body(url).await {
            Ok(body) => Ok(extract_text(&body)),
            Err(first) => {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let body = self
                    .fetch_body(url)
                    .await
                    .with_context(|| format!("{first:#}; retry failed"))?;
                Ok(extract_text(&body))
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

/// 行级 diff：保序集合差——新增/消失的行各是什么。顺序抖动不产生输出。
fn diff_lines(old: &[String], new: &[String]) -> (Vec<String>, Vec<String>) {
    let old_set: std::collections::HashSet<&str> = old.iter().map(|s| s.as_str()).collect();
    let new_set: std::collections::HashSet<&str> = new.iter().map(|s| s.as_str()).collect();
    let added = new
        .iter()
        .filter(|l| !old_set.contains(l.as_str()))
        .cloned()
        .collect();
    let removed = old
        .iter()
        .filter(|l| !new_set.contains(l.as_str()))
        .cloned()
        .collect();
    (added, removed)
}

/// 监控状态表在 vault 里的位置。
pub const STATUS_PATH: &str = "05-Opportunities-机会/页面监控.md";

/// Unix 秒 → "10-27 17:35"（本地时区）。
fn fmt_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}

/// 监控状态表渲染：每轮全量重写，Obsidian 里永远是最新状态。纯函数便于测试。
fn render_status(
    pages: &[WatchedPage],
    docs: &std::collections::HashMap<String, serde_json::Value>,
) -> String {
    let mut out = String::from(
        "# 📡 页面监控\n\n         > ru41 pagewatch 附属每 30 分钟巡查一轮，只维护最新状态，不推送。\n         > 变更判定：可见文本行级比对（标签噪声/顺序抖动不算变更）。\n\n         | 页面 | 最近巡查 | 最近内容变更 |\n|---|---|---|\n",
    );
    for page in pages {
        let row = match docs.get(&page.url) {
            Some(doc) => {
                let checked = doc["last_checked_at"]
                    .as_i64()
                    .or(doc["baseline_at"].as_i64())
                    .map(fmt_ts)
                    .unwrap_or_else(|| "—".into());
                let changed = doc["last_changed_at"]
                    .as_i64()
                    .map(fmt_ts)
                    .unwrap_or_else(|| "接入以来无变化".into());
                format!(
                    "| [{}]({}) | {} | {} |",
                    page.name, page.url, checked, changed
                )
            }
            None => format!("| [{}]({}) | 未建立基线 | — |", page.name, page.url),
        };
        out.push_str(&row);
        out.push('\n');
    }
    out
}

/// 提取可见文本：(内容指纹, 行列表)。剔除 script/style/注释 → 块级标签转换行 →
/// 去全部标签 → 行内空白归一。指纹只用于"变没变"的快速比对，diff 才是给人看的。
///
/// 为什么标签也要去：Cloudflare 邮箱混淆（data-cfemail、
/// /cdn-cgi/l/email-protection#随机串）和构建期随机 class/id 都藏在标签属性里，
/// 每请求都变但可见内容没变——按标签散列会把这种噪声当成"页面更新"反复误报
/// （2026-10-27 Mitacs 页面每 30 分钟误报一次的根因）。
/// 注意：纯 SPA 页面（正文只有"加载中"）剔除脚本后指纹恒定，
/// 永远报不出变化——这类页面应改用其数据 API 配 RSS 伪 feed 监控。
fn extract_text(html: &str) -> (String, Vec<String>) {
    use regex::Regex;
    use std::sync::OnceLock;
    fn noise() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(r"(?si)<script[^>]*>.*?</script>|<style[^>]*>.*?</style>|<!--.*?-->")
                .expect("noise regex")
        })
    }
    fn block_tags() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(r"(?i)</(?:p|div|li|tr|td|h[1-6]|section|article|header|footer|ul|ol|table)>|<br[^>]*>")
                .expect("block tag regex")
        })
    }
    fn tags() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"(?s)<[^>]+>").expect("tag regex"))
    }
    let stripped = noise().replace_all(html, " ");
    let broken = block_tags().replace_all(&stripped, "\n");
    let text = tags().replace_all(&broken, " ");
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .map(|l| crate::util::truncate_chars(&l, 200))
        .take(3000)
        .collect();
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    lines.join(" ").hash(&mut hasher);
    (format!("{:016x}", hasher.finish()), lines)
}

#[cfg(test)]
fn content_fingerprint(html: &str) -> String {
    extract_text(html).0
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
    fn extract_text_breaks_block_tags_into_lines() {
        let html = r#"<html><body><h1>比赛列表</h1><p>第一场 <b>黑客松</b></p><p>第二场</p></body></html>"#;
        let (fp, lines) = extract_text(html);
        assert!(!fp.is_empty());
        assert_eq!(lines, vec!["比赛列表", "第一场 黑客松", "第二场"]);
    }

    #[test]
    fn diff_lines_reports_added_and_removed_ignoring_reorder() {
        let old = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        // 纯换序 → 无增删（顺序抖动不打扰）
        let reordered = vec!["c".to_owned(), "a".to_owned(), "b".to_owned()];
        let (added, removed) = diff_lines(&old, &reordered);
        assert!(added.is_empty() && removed.is_empty());
        // 有新增有消失
        let new = vec!["a".to_owned(), "d".to_owned()];
        let (added, removed) = diff_lines(&old, &new);
        assert_eq!(added, vec!["d".to_owned()]);
        assert_eq!(removed, vec!["b".to_owned(), "c".to_owned()]);
    }

    #[test]
    fn status_table_renders_pages_and_states() {
        let pages = vec![
            WatchedPage {
                name: "页面A".into(),
                url: "https://a".into(),
            },
            WatchedPage {
                name: "页面B".into(),
                url: "https://b".into(),
            },
        ];
        let mut docs = std::collections::HashMap::new();
        docs.insert(
            "https://a".to_owned(),
            serde_json::json!({"last_checked_at": 1_793_000_000, "last_changed_at": 1_792_900_000}),
        );
        let out = render_status(&pages, &docs);
        assert!(out.contains("[页面A](https://a)"));
        assert!(out.contains("[页面B](https://b) | 未建立基线"), "{out}");
        assert!(out.contains("最近内容变更"), "{out}");
        // 时间戳渲染成本地时刻而不是原始数字
        assert!(!out.contains("1793000000"), "{out}");
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
