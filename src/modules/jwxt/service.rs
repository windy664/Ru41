use crate::ports::{channel::NotifyChannel, storage::TaskStore};
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate, Timelike};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;

use super::source::{CourseMeeting, JwxtClient, JwxtError};

/// 教务课表监控配置（对应配置文件的 [sources.jwxt]）。
#[derive(Debug, Clone)]
pub struct JwxtConfig {
    pub username: String,
    pub password: String,
    pub cas_base: String,
    pub cas_modulus: String,
    pub cas_exponent: String,
    pub jwxt_base: String,
    /// 会话持久化路径（TGT + cookie）。
    pub session_path: std::path::PathBuf,
    /// 学期第一周周一；周次推算与明日课表依赖它，缺省时只做变更检测。
    pub semester_start: Option<NaiveDate>,
    /// 学年起始年；None 时按日期推断。
    pub xnm: Option<String>,
    /// 学期码；None 时按日期推断。
    pub xqm: Option<String>,
    pub interval_secs: u64,
    /// 是否推送明日课表。
    pub daily_remind: bool,
    /// 明日课表推送的本地时间下限（小时）。
    pub remind_hour: u8,
    /// 提醒投递目标（装配层按 [notify] 渠道解析）。
    pub notify_target: String,
}

/// 按日期推断当前学期：9-12 月 → (当年, 3)；1 月 → (去年, 3)；2-8 月 → (去年, 12)。
/// 短学期（xqm=16）不参与自动推断，需要时配置里显式指定。
pub fn infer_term(today: NaiveDate) -> (String, String) {
    let (year, month) = (today.year(), today.month());
    match month {
        9..=12 => (year.to_string(), "3".to_owned()),
        1 => ((year - 1).to_string(), "3".to_owned()),
        _ => ((year - 1).to_string(), "12".to_owned()),
    }
}

/// 配置优先的学期解析。
pub fn resolve_term(xnm: Option<&str>, xqm: Option<&str>, today: NaiveDate) -> (String, String) {
    let (inferred_xnm, inferred_xqm) = infer_term(today);
    (
        xnm.unwrap_or(&inferred_xnm).to_owned(),
        xqm.unwrap_or(&inferred_xqm).to_owned(),
    )
}

/// 学期第几周（semester_start 为第一周周一）；开学前返回 0。
pub fn week_of(semester_start: NaiveDate, date: NaiveDate) -> i64 {
    (date - semester_start).num_days().div_euclid(7) + 1
}

/// 日期 → 正方星期号（周一=1 … 周日=7）。
fn weekday_no(date: NaiveDate) -> u8 {
    (date.weekday().number_from_monday()) as u8
}

/// 取某天要上的课（星期匹配 + 周次在 zcd 范围内）。
pub fn meetings_on(
    meetings: &[CourseMeeting],
    semester_start: NaiveDate,
    date: NaiveDate,
) -> Vec<CourseMeeting> {
    let week = week_of(semester_start, date);
    let weekday = weekday_no(date);
    meetings
        .iter()
        .filter(|m| m.weekday == weekday && m.weeks.contains(&(week as u32)))
        .cloned()
        .collect()
}

/// 一轮轮询的报告。
#[derive(Debug, Serialize)]
pub struct JwxtPollReport {
    pub login_ok: bool,
    pub xnm: String,
    pub xqm: String,
    /// 课表行数（整学期）。
    pub meetings: usize,
    /// 当前学期周次（未配置 semester_start 时为 null）。
    pub week: Option<i64>,
    /// 本轮检测到的课表变更条数。
    pub changes: usize,
    /// 是否已推送今日明日课表提醒。
    pub daily_reminded: bool,
    pub notified: bool,
}

/// 课表快照记录（jwxt_timetable 的 value）。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TimetableSnapshot {
    xnm: String,
    xqm: String,
    hash: u64,
    updated_at: i64,
    meetings: Vec<StoredMeeting>,
}

/// 快照里的行课条目（CourseMeeting 的 JSON 形态，QQ 命令也读这一份）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredMeeting {
    pub course: String,
    pub course_id: String,
    pub teacher: String,
    pub classroom: String,
    pub weekday: u8,
    pub sections_raw: String,
    pub start_section: Option<u8>,
    pub end_section: Option<u8>,
    pub weeks_raw: String,
    pub weeks: Vec<u32>,
    pub class_type: String,
    pub class_group: String,
}

impl From<&CourseMeeting> for StoredMeeting {
    fn from(m: &CourseMeeting) -> Self {
        Self {
            course: m.course.clone(),
            course_id: m.course_id.clone(),
            teacher: m.teacher.clone(),
            classroom: m.classroom.clone(),
            weekday: m.weekday,
            sections_raw: m.sections_raw.clone(),
            start_section: m.start_section,
            end_section: m.end_section,
            weeks_raw: m.weeks_raw.clone(),
            weeks: m.weeks.clone(),
            class_type: m.class_type.clone(),
            class_group: m.class_group.clone(),
        }
    }
}

impl StoredMeeting {
    /// 提醒/回复里的一行：`9-10节 军事理论 @T301（鲁鲜亮）`。
    pub fn display_line(&self) -> String {
        let section = match (self.start_section, self.end_section) {
            (Some(s), Some(e)) if s == e => format!("{s}节"),
            (Some(s), Some(e)) => format!("{s}-{e}节"),
            _ => self.sections_raw.clone(),
        };
        let teacher = if self.teacher.is_empty() {
            String::new()
        } else {
            format!("（{}）", self.teacher)
        };
        let room = if self.classroom.is_empty() {
            "待定".to_owned()
        } else {
            self.classroom.clone()
        };
        format!("{section} {} @{room}{teacher}", self.course)
    }
}

fn signature_hash(signatures: &[String]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for sig in signatures {
        sig.hash(&mut hasher);
    }
    hasher.finish()
}

/// 教务课表监控：CAS 会话 → 整学期课表 → 快照 diff（调课/停课提醒）→
/// 到点推送明日课表。会话失效只提醒（每日最多一次），不自动重试验证码。
pub struct JwxtTracker {
    store: Arc<dyn TaskStore>,
    notifier: Option<Arc<dyn NotifyChannel>>,
    client: JwxtClient,
    config: JwxtConfig,
}

impl JwxtTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        client: JwxtClient,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: JwxtConfig,
    ) -> Self {
        Self {
            store,
            notifier,
            client,
            config,
        }
    }

    /// 供 login 子命令拿到客户端。
    pub fn client(&self) -> &JwxtClient {
        &self.client
    }

    /// 登录失效提醒：同日去重（jwxt_alert key = login:{date}）。
    async fn alert_login_failure(&self, err: &JwxtError) -> Result<bool> {
        let now = crate::util::now();
        let today = crate::util::today_utc(now);
        let key = format!("login:{today}");
        if self.store.document("jwxt_alert", &key)?.is_some() {
            return Ok(false);
        }
        self.store.put_document(
            "jwxt_alert",
            &key,
            None,
            &serde_json::json!({ "ts": now, "error": err.to_string() }),
        )?;
        if let Some(notifier) = &self.notifier {
            notifier
                .send(
                    &self.config.notify_target,
                    None,
                    &format!("⚠️ 教务登录态失效：{err}\n请在服务器运行 `ru41 jwxt login --captcha <答案>` 重新登录"),
                )
                .await?;
        }
        Ok(true)
    }

    /// 课表快照 diff：返回变更行（新增/取消），并写回新快照。
    async fn diff_and_store(
        &self,
        xnm: &str,
        xqm: &str,
        meetings: &[CourseMeeting],
    ) -> Result<Vec<String>> {
        let key = format!("{xnm}#{xqm}");
        let now = crate::util::now();
        let signatures: Vec<String> = meetings.iter().map(CourseMeeting::signature).collect();
        let hash = signature_hash(&signatures);
        let stored: Vec<StoredMeeting> = meetings.iter().map(StoredMeeting::from).collect();

        let existing = self.store.document("jwxt_timetable", &key)?;
        let Some(doc) = existing else {
            self.store.put_document(
                "jwxt_timetable",
                &key,
                None,
                &serde_json::to_value(TimetableSnapshot {
                    xnm: xnm.to_owned(),
                    xqm: xqm.to_owned(),
                    hash,
                    updated_at: now,
                    meetings: stored,
                })?,
            )?;
            return Ok(Vec::new());
        };

        let old: TimetableSnapshot =
            serde_json::from_value(doc.value.clone()).unwrap_or_else(|_| TimetableSnapshot {
                xnm: xnm.to_owned(),
                xqm: xqm.to_owned(),
                hash: 0,
                updated_at: 0,
                meetings: Vec::new(),
            });
        if old.hash == hash {
            return Ok(Vec::new());
        }

        let old_sigs: HashSet<String> = old.meetings.iter().map(stored_signature).collect();
        let mut changes = Vec::new();
        for meeting in meetings {
            let sig = &meeting.signature();
            if !old_sigs.contains(sig) {
                changes.push(format!(
                    "+ 新增 {}｜{}｜星期{}｜{}｜@{}",
                    meeting.course,
                    meeting.weeks_raw,
                    meeting.weekday,
                    meeting.sections_raw,
                    meeting.classroom
                ));
            }
        }
        let new_sigs: HashSet<String> = signatures.iter().cloned().collect();
        for meeting in &old.meetings {
            if !new_sigs.contains(stored_signature(meeting).as_str()) {
                changes.push(format!(
                    "- 取消 {}｜{}｜星期{}｜{}｜@{}",
                    meeting.course,
                    meeting.weeks_raw,
                    meeting.weekday,
                    meeting.sections_raw,
                    meeting.classroom
                ));
            }
        }

        self.store.put_document(
            "jwxt_timetable",
            &key,
            Some(doc.revision),
            &serde_json::to_value(TimetableSnapshot {
                xnm: xnm.to_owned(),
                xqm: xqm.to_owned(),
                hash,
                updated_at: now,
                meetings: stored,
            })?,
        )?;
        Ok(changes)
    }

    /// 明日课表提醒：到达 remind_hour 后每日一次（jwxt_alert key = daily:{date}）。
    async fn maybe_daily_remind(
        &self,
        meetings: &[CourseMeeting],
        today: NaiveDate,
    ) -> Result<bool> {
        if !self.config.daily_remind {
            return Ok(false);
        }
        let now_local = chrono::Local::now();
        if now_local.hour() < self.config.remind_hour as u32 {
            return Ok(false);
        }
        let today_utc = crate::util::today_utc(crate::util::now());
        let key = format!("daily:{today_utc}");
        if self.store.document("jwxt_alert", &key)?.is_some() {
            return Ok(false);
        }
        self.store.put_document(
            "jwxt_alert",
            &key,
            None,
            &serde_json::json!({ "ts": crate::util::now() }),
        )?;
        let Some(notifier) = &self.notifier else {
            return Ok(true);
        };
        let Some(start) = self.config.semester_start else {
            notifier
                .send(
                    &self.config.notify_target,
                    None,
                    "⚠️ 未配置 sources.jwxt.semester_start（学期第一周周一），无法推送明日课表",
                )
                .await?;
            return Ok(true);
        };
        let tomorrow = today + Duration::days(1);
        let day_meetings = meetings_on(meetings, start, tomorrow);
        let message = format!(
            "【明日课表】{}（第{}周）\n{}",
            tomorrow.format("%m月%d日 %A"),
            week_of(start, tomorrow),
            if day_meetings.is_empty() {
                "明天没有课 🎉".to_owned()
            } else {
                let mut lines: Vec<String> = day_meetings
                    .iter()
                    .map(|m| StoredMeeting::from(m).display_line())
                    .collect();
                lines.sort();
                format!("{}\n共 {} 节课", lines.join("\n"), lines.len())
            }
        );
        notifier
            .send(&self.config.notify_target, None, &message)
            .await?;
        Ok(true)
    }

    pub async fn poll_once(&self) -> Result<JwxtPollReport> {
        let today = chrono::Local::now().date_naive();
        let (xnm, xqm) = resolve_term(
            self.config.xnm.as_deref(),
            self.config.xqm.as_deref(),
            today,
        );

        if let Err(err) = self.client.ensure_session().await {
            let alerted = self.alert_login_failure(&err).await?;
            return Ok(JwxtPollReport {
                login_ok: false,
                xnm,
                xqm,
                meetings: 0,
                week: self
                    .config
                    .semester_start
                    .map(|start| week_of(start, today)),
                changes: 0,
                daily_reminded: false,
                notified: alerted,
            });
        }

        let meetings = match self.client.timetable(&xnm, &xqm).await {
            Ok(meetings) => meetings,
            Err(err) => {
                let alerted = self.alert_login_failure(&err).await?;
                return Ok(JwxtPollReport {
                    login_ok: false,
                    xnm,
                    xqm,
                    meetings: 0,
                    week: self
                        .config
                        .semester_start
                        .map(|start| week_of(start, today)),
                    changes: 0,
                    daily_reminded: false,
                    notified: alerted,
                });
            }
        };

        let changes = self.diff_and_store(&xnm, &xqm, &meetings).await?;
        if !changes.is_empty()
            && let Some(notifier) = &self.notifier
        {
            notifier
                .send(
                    &self.config.notify_target,
                    None,
                    &format!("【课表变更】{xnm}-{xqm} 学期\n{}", changes.join("\n")),
                )
                .await?;
        }
        let daily_reminded = self.maybe_daily_remind(&meetings, today).await?;

        Ok(JwxtPollReport {
            login_ok: true,
            xnm,
            xqm,
            meetings: meetings.len(),
            week: self
                .config
                .semester_start
                .map(|start| week_of(start, today)),
            changes: changes.len(),
            daily_reminded,
            notified: !changes.is_empty() || daily_reminded,
        })
    }
}

/// StoredMeeting 的签名（与 CourseMeeting::signature 同构，便于 diff）。
fn stored_signature(meeting: &StoredMeeting) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        meeting.course_id,
        meeting.weekday,
        meeting.sections_raw,
        meeting.weeks_raw,
        meeting.classroom,
        meeting.teacher
    )
}

/// 从缓存快照构造某天的课表回复（QQ 命令用；不触网）。
pub fn describe_day_from_store(
    store: &Arc<dyn TaskStore>,
    semester_start: Option<NaiveDate>,
    date: NaiveDate,
) -> Result<String> {
    let (xnm, xqm) = resolve_term(None, None, chrono::Local::now().date_naive());
    let key = format!("{xnm}#{xqm}");
    let Some(doc) = store.document("jwxt_timetable", &key)? else {
        return Ok("## 暂无课表缓存\n\n请先在服务器运行 `jwxt poll`。".to_owned());
    };
    let snapshot: TimetableSnapshot = serde_json::from_value(doc.value)?;
    let Some(start) = semester_start else {
        return Ok(
            "## 缺少学期配置\n\n请在配置里设置 `sources.jwxt.semester_start`（学期第一周周一），才能按周次过滤课表。"
                .to_owned(),
        );
    };
    let meetings: Vec<CourseMeeting> = snapshot
        .meetings
        .iter()
        .map(|m| CourseMeeting {
            course: m.course.clone(),
            course_id: m.course_id.clone(),
            teacher: m.teacher.clone(),
            classroom: m.classroom.clone(),
            weekday: m.weekday,
            sections_raw: m.sections_raw.clone(),
            start_section: m.start_section,
            end_section: m.end_section,
            weeks: m.weeks.clone(),
            weeks_raw: m.weeks_raw.clone(),
            class_type: m.class_type.clone(),
            class_group: m.class_group.clone(),
            xnm: snapshot.xnm.clone(),
            xqm: snapshot.xqm.clone(),
        })
        .collect();
    let day_meetings = meetings_on(&meetings, start, date);
    let title = format!(
        "{}（第{}周 星期{}）",
        date.format("%m月%d日"),
        week_of(start, date),
        weekday_no(date)
    );
    let body = if day_meetings.is_empty() {
        "没有课 🎉".to_owned()
    } else {
        let mut lines: Vec<String> = day_meetings
            .iter()
            .map(|m| StoredMeeting::from(m).display_line())
            .collect();
        lines.sort();
        lines.join("\n")
    };
    Ok(format!("## {title}\n\n{body}"))
}

impl crate::ingress::schedule::PollJob for JwxtTracker {
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
