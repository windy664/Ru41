use anyhow::Result;
use chrono::NaiveDate;
use ru41::{
    ports::{channel::NotifyChannel, storage::TaskStore},
    ports::commands::{MarkupButton, ReplyMarkup},
};
use serde::Serialize;
use std::sync::Arc;

use crate::source::{LibraryClient, LoanRecord};

/// 图书馆借阅监控配置（对应配置文件的 [sources.library]）。
#[derive(Debug, Clone)]
pub struct LibraryConfig {
    pub username: String,
    pub password: String,
    pub cas_base: String,
    pub cas_modulus: String,
    pub cas_exponent: String,
    pub unified_base: String,
    pub find_base: String,
    pub mobile_base: String,
    pub group_code: String,
    pub session_path: std::path::PathBuf,
    pub interval_secs: u64,
    /// 应还前 N 天开始每日提醒。
    pub remind_days: i64,
    /// 提醒投递目标（装配层按 [notify] 渠道解析）。
    pub notify_target: String,
}

/// 一轮轮询的报告。
#[derive(Debug, Serialize)]
pub struct LibraryPollReport {
    pub loans: usize,
    /// 剩余天数 <= remind_days 的书目。
    pub due_soon: Vec<String>,
    /// 已过期书目。
    pub overdue: Vec<String>,
    pub login_ok: bool,
    pub notified: bool,
    /// 追书到货书目（可借数 0→N）。
    pub watch_alerts: Vec<String>,
}

/// 按剩余天数分级；纯函数便于测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DueLevel {
    /// 已过期。
    Overdue,
    /// 今天到期。
    Today,
    /// 明天到期。
    Tomorrow,
    /// remind_days 窗口内。
    DueSoon,
    /// 窗口外，不打扰。
    Quiet,
}

/// 应还日期（YYYY-MM-DD 前缀，兼容带时间的串）与今天的差（天）。
/// 解析失败返回 None（不打扰，但报告中可见）。
pub fn days_until_due(norm_return_date: Option<&str>, today: NaiveDate) -> Option<i64> {
    let raw = norm_return_date?.trim().get(..10)?;
    let due = NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    Some((due - today).num_days())
}

pub fn due_level(days_left: i64, remind_days: i64) -> DueLevel {
    if days_left < 0 {
        DueLevel::Overdue
    } else if days_left == 0 {
        DueLevel::Today
    } else if days_left == 1 {
        DueLevel::Tomorrow
    } else if days_left <= remind_days {
        DueLevel::DueSoon
    } else {
        DueLevel::Quiet
    }
}

/// 今天（本地时区；应还日期按北京时间语义）。
pub fn local_today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// 图书借阅监控：会话保活 → 拉当前借阅 → 应还日期分级（纯 Rust 规则，
/// 不经 Agent）→ 临近到期经注入的通知渠道每日提醒一次。
///
/// 通知策略：
/// - 剩余天数 <= remind_days（默认 3）或已过期 → 每书每日最多提醒一次，
///   文案按 逾期/今天/明天/一般 递进；
/// - 登录态失效（自动登录被拒等硬失败）→ 每日最多提醒一次。
#[derive(Clone)]
pub struct LibraryTracker {
    store: Arc<dyn TaskStore>,
    client: LibraryClient,
    notifier: Option<Arc<dyn NotifyChannel>>,
    config: LibraryConfig,
}

impl LibraryTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        client: LibraryClient,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: LibraryConfig,
    ) -> Self {
        Self {
            store,
            client,
            notifier,
            config,
        }
    }

    pub async fn poll_once(&self) -> Result<LibraryPollReport> {
        let now = ru41::util::now();
        let today = local_today();

        // 追书到货检测走公开馆藏接口，不依赖登录态（登录挂了照样追书）。
        // 失败只记日志，不拖垮催还主流程。
        let watch_alerts = match self.check_watches(now).await {
            Ok(alerts) => alerts,
            Err(err) => {
                eprintln!("[library] 追书检测失败: {err:#}");
                Vec::new()
            }
        };

        let page = match self.client.ensure_session().await {
            Err(err) => return self.login_failed_report(err, now, watch_alerts).await,
            Ok(()) => match self.client.loans().await {
                Ok(page) => page,
                Err(err) => return self.login_failed_report(err, now, watch_alerts).await,
            },
        };

        let mut due_soon = Vec::new();
        let mut overdue = Vec::new();
        let mut lines: Vec<String> = Vec::new();
        // 催还消息附带的续借按钮（每书一个，QQ 键盘单行最多 4 个）。
        let mut renew_buttons: Vec<(String, String)> = Vec::new();
        for record in &page.records {
            let Some(days_left) = days_until_due(record.norm_return_date.as_deref(), today) else {
                continue;
            };
            let level = due_level(days_left, self.config.remind_days);
            if level == DueLevel::Quiet {
                continue;
            }
            let line = format_loan_line(record, days_left);
            if level == DueLevel::Overdue {
                overdue.push(record.display_title());
            } else {
                due_soon.push(record.display_title());
            }
            // 每书每日最多提醒一次。
            let nag_key = format!("{}:{}", record.key(), today.format("%Y-%m-%d"));
            if self.store.document("library_alert", &nag_key)?.is_some() {
                continue;
            }
            renew_buttons.push((
                // 按钮文案截断防书名过长撑爆键盘；命令带全名保证续借精确匹配。
                format!("🔄 续借《{}》", truncate_label(&record.display_title(), 6)),
                format!("/续借 {}", record.display_title()),
            ));
            if self.notifier.is_some() {
                self.store.put_document(
                    "library_alert",
                    &nag_key,
                    None,
                    &serde_json::json!({ "ts": now, "due": record.norm_return_date }),
                )?;
                lines.push(line);
            }
        }

        let mut notified = false;
        if !lines.is_empty()
            && let Some(notifier) = &self.notifier
        {
            let mut card = ru41::util::msg::Card::new("📚 图书催还");
            for line in &lines {
                card.raw(line);
            }
            card.hint("点下方按钮直接续借，或回复 `/借阅` 查看在借清单");
            let message = card.build();
            let mut markup = ru41::ports::commands::ReplyMarkup::new();
            markup.push_row(
                renew_buttons
                    .iter()
                    .take(4)
                    .map(|(label, cmd)| {
                        ru41::ports::commands::MarkupButton::new(label.clone(), cmd.clone())
                    })
                    .collect(),
            );
            markup.push_row(vec![ru41::ports::commands::MarkupButton::new(
                "📚 在借清单",
                "/借阅",
            )]);
            notifier
                .send_with_markup(&self.config.notify_target, None, &message, &markup)
                .await?;
            notified = true;
        }

        Ok(LibraryPollReport {
            loans: page.records.len(),
            due_soon,
            overdue,
            login_ok: true,
            notified,
            watch_alerts,
        })
    }

    /// 追书到货检测（公开馆藏接口，不依赖登录态）：
    /// 每条活跃追书查一次 groupitems，可借数（在架且非样本）0→N 触发通知。
    async fn check_watches(&self, now: i64) -> Result<Vec<String>> {
        let mut watches = self.client.load_watches();
        if !watches.iter().any(|w| w.active) {
            return Ok(Vec::new());
        }
        let mut alerted = Vec::new();
        let mut dirty = false;
        for watch in &mut watches {
            if !watch.active {
                continue;
            }
            let items = match self.client.holdings(watch.record_id).await {
                Ok(items) => items,
                Err(err) => {
                    eprintln!("[library] 追书《{}》馆藏查询失败: {err:#}", watch.title);
                    continue;
                }
            };
            let loanable = items
                .iter()
                .filter(|h| h.is_on_shelf() && !h.is_sample())
                .count() as i64;
            if watch.last_loanable == 0 && loanable > 0 {
                if let Some(notifier) = &self.notifier {
                    let locations: Vec<String> = items
                        .iter()
                        .filter(|h| h.is_on_shelf() && !h.is_sample())
                        .filter_map(|h| h.location_name.clone())
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    notifier
                        .send(
                            &self.config.notify_target,
                            None,
                            &format!(
                                "## 📖 追书到货\n\n《{}》可以借了！\n\n- 索书号 {}\n- 可借 {loanable} 册（{}）\n\n> 借到后回复 `/追书 取消 {}` 停止跟踪",
                                watch.title,
                                watch.call_no.as_deref().unwrap_or("见详情页"),
                                locations.join("、"),
                                watch.record_id
                            ),
                        )
                        .await?;
                }
                alerted.push(watch.title.clone());
            }
            watch.last_loanable = loanable;
            watch.last_check = now;
            dirty = true;
        }
        if dirty {
            self.client.save_watches(&watches)?;
        }
        Ok(alerted)
    }

    /// 登录失败：每日最多提醒一次（PASSERROR 锁号等硬失败同样只提醒人工）。
    async fn login_failed_report(
        &self,
        err: anyhow::Error,
        now: i64,
        watch_alerts: Vec<String>,
    ) -> Result<LibraryPollReport> {
        eprintln!("[library] 自动登录失败: {err:#}");
        let today = local_today().format("%Y-%m-%d").to_string();
        let key = format!("login:{today}");
        let mut notified = false;
        if self.store.document("library_alert", &key)?.is_none() {
            if let Some(notifier) = &self.notifier {
                self.store.put_document(
                    "library_alert",
                    &key,
                    None,
                    &serde_json::json!({ "ts": now }),
                )?;
                notifier
                    .send(
                        &self.config.notify_target,
                        None,
                        &format!(
                            "## ⚠️ 图书馆登录态失效\n\n自动登录失败：{err}\n\n恢复前借阅提醒不可用。"
                        ),
                    )
                    .await?;
                notified = true;
            }
        }
        Ok(LibraryPollReport {
            loans: 0,
            due_soon: Vec::new(),
            overdue: Vec::new(),
            login_ok: false,
            notified,
            watch_alerts,
        })
    }
}

/// 单条催还提醒行。
/// 按钮文案截断：按字符数截断并补省略号。
fn truncate_label(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

pub fn format_loan_line(record: &LoanRecord, days_left: i64) -> String {
    let due = record
        .norm_return_date
        .as_deref()
        .map(|d| d.trim().get(..10).unwrap_or(d))
        .unwrap_or("未知");
    let tag = if days_left < 0 {
        format!("❌ 已过期 {} 天", -days_left)
    } else if days_left == 0 {
        "🔴 今天到期".to_owned()
    } else if days_left == 1 {
        "⚠️ 明天到期".to_owned()
    } else {
        format!("还有 {days_left} 天")
    };
    let location = record
        .location_name
        .as_deref()
        .filter(|l| !l.is_empty())
        .map(|l| format!("｜{l}"))
        .unwrap_or_default();
    format!(
        "- 《{}》｜应还 {due}（{tag}）{location}",
        record.display_title()
    )
}

/// /借阅 命令的回复排版 + 续借按钮（每本一行按钮，最多 8 行防刷屏）。
pub fn format_loan_reply(page: &crate::source::LoanPage) -> ReplyMarkup {
    let mut markup = ReplyMarkup::new();
    for record in page.records.iter().take(8) {
        let title = record.display_title();
        let short: String = title.chars().take(12).collect();
        markup.push_row(vec![MarkupButton::new(
            format!("🔄续借 {short}"),
            format!("/续借 {title}"),
        )]);
    }
    markup
}

impl ru41::ingress::schedule::PollJob for LibraryTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        let owned = self.clone();
        ru41::plugins::bridge(async move { Ok(serde_json::to_value(owned.poll_once().await?)?) })
    }
}

#[cfg(test)]
mod tests {
    use super::{DueLevel, days_until_due, due_level, format_loan_line};
    use crate::source::LoanRecord;
    use chrono::NaiveDate;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
    }

    fn record(due: &str) -> LoanRecord {
        LoanRecord {
            title: Some("算法导论".into()),
            norm_return_date: Some(due.into()),
            location_name: Some("计算机书库".into()),
            ..Default::default()
        }
    }

    #[test]
    fn level_boundaries() {
        assert_eq!(due_level(-2, 3), DueLevel::Overdue);
        assert_eq!(due_level(0, 3), DueLevel::Today);
        assert_eq!(due_level(1, 3), DueLevel::Tomorrow);
        assert_eq!(due_level(2, 3), DueLevel::DueSoon);
        assert_eq!(due_level(3, 3), DueLevel::DueSoon);
        assert_eq!(due_level(4, 3), DueLevel::Quiet);
    }

    #[test]
    fn days_until_due_parses_date_prefix() {
        assert_eq!(days_until_due(Some("2026-11-04"), today()), Some(3));
        assert_eq!(
            days_until_due(Some("2026-11-04 21:00:00"), today()),
            Some(3)
        );
        assert_eq!(days_until_due(Some("2026-10-30"), today()), Some(-2));
        assert_eq!(days_until_due(None, today()), None);
        assert_eq!(days_until_due(Some("垃圾数据"), today()), None);
    }

    #[test]
    fn loan_line_escalation_wording() {
        let line = format_loan_line(&record("2026-10-28"), -4);
        assert!(line.contains("❌ 已过期 4 天"), "{line}");
        assert!(line.contains("应还 2026-10-28"));
        assert!(line.contains("计算机书库"));
        assert!(format_loan_line(&record("2026-11-01"), 0).contains("🔴 今天到期"));
        assert!(format_loan_line(&record("2026-11-02"), 1).contains("⚠️ 明天到期"));
        assert!(format_loan_line(&record("2026-11-04"), 3).contains("还有 3 天"));
    }
}
