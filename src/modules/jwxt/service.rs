use crate::ports::{channel::NotifyChannel, storage::TaskStore};
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Timelike};
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
    /// 是否把课表同步进日程系统（课前 20 分钟提醒、`/日程` 可见）。
    pub sync_schedule: bool,
    /// 节次时刻表；日程同步与消息里的具体时间显示依赖它。
    pub clock: Option<SectionClock>,
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

const WEEKDAY_NAMES: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
fn weekday_name(date: NaiveDate) -> &'static str {
    WEEKDAY_NAMES[(date.weekday().number_from_monday() - 1) as usize]
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

/// 节次时刻表：把 "3-4节" 换算成 "10:40-12:00"。教务接口不暴露作息时间，
/// 由配置 `section_times` + `class_duration_mins` 提供。
#[derive(Debug, Clone)]
pub struct SectionClock {
    /// 索引 = 节次 - 1。
    pub times: Vec<chrono::NaiveTime>,
    /// 单节时长（分钟）。
    pub duration_mins: u64,
}

impl SectionClock {
    /// 节次范围 → "HH:MM-HH:MM"；超出时刻表范围返回 None（调用方回退到节次文本）。
    pub fn range_label(&self, start: Option<u8>, end: Option<u8>) -> Option<String> {
        let (s, e) = (start?, end?);
        let start_t = *self.times.get(s as usize - 1)?;
        let end_t = *self.times.get(e as usize - 1)?;
        let end_t = end_t + Duration::minutes(self.duration_mins as i64);
        Some(format!(
            "{}-{}",
            start_t.format("%H:%M"),
            end_t.format("%H:%M")
        ))
    }
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
    /// 本轮日程同步：新建事件数。
    pub events_created: usize,
    /// 本轮日程同步：取消事件数（调课/停课导致）。
    pub events_cancelled: usize,
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
    /// 提醒/回复里的一行：`- **10:40-12:00** 高等数学 @U102（张晓婷）`。
    /// 未配置作息表时回退到节次文本（`3-4节`）。
    pub fn display_line(&self, clock: Option<&SectionClock>) -> String {
        let time = clock
            .and_then(|c| c.range_label(self.start_section, self.end_section))
            .unwrap_or_else(|| match (self.start_section, self.end_section) {
                (Some(s), Some(e)) if s == e => format!("{s}节"),
                (Some(s), Some(e)) => format!("{s}-{e}节"),
                _ => self.sections_raw.clone(),
            });
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
        format!("- **{time}** {} @{room}{teacher}", self.course)
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

    /// 登录失效提醒：课表是学期级快照、每日刷新即可，单日故障不值得打扰——
    /// 只有「当前学期完全没有课表缓存」（从未成功过）或「连续失败 ≥3 天」
    /// （数据开始变陈）才提醒，且同日去重（jwxt_alert key = login:{date}）。
    /// 其余故障静默重试，恢复时由 mark_login_state 报一次自愈。
    async fn alert_login_failure(&self, err: &JwxtError, term_key: &str) -> Result<bool> {
        const ALERT_AFTER_DAYS: i64 = 3;
        let now = crate::util::now();
        let today = crate::util::today_utc(now);
        // 连续失败天数按天累计（同日多次轮询只算一次）。
        let streak_doc = self.store.document("jwxt_alert", "login_streak")?;
        let (mut days, last_date) = streak_doc
            .as_ref()
            .map(|d| {
                (
                    d.value["days"].as_i64().unwrap_or(0),
                    d.value["last_date"].as_str().unwrap_or("").to_owned(),
                )
            })
            .unwrap_or((0, String::new()));
        if last_date != today {
            days += 1;
        }
        self.store.put_document(
            "jwxt_alert",
            "login_streak",
            streak_doc.as_ref().map(|d| d.revision),
            &serde_json::json!({ "days": days, "last_date": today }),
        )?;
        let has_cache = self.store.document("jwxt_timetable", term_key)?.is_some();
        if has_cache && days < ALERT_AFTER_DAYS {
            return Ok(false);
        }
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
            let reason = if !has_cache {
                "当前学期还没有课表缓存".to_owned()
            } else {
                format!("已连续失败 {days} 天，课表快照在变陈")
            };
            notifier
                .send(
                    &self.config.notify_target,
                    None,
                    &format!(
                        "⚠️ 教务登录失败（{reason}）：{err}\n若是密码/账号问题请检查配置；若验证码识别失效，可手动 `ru41 jwxt login --captcha <答案>` 兜底"
                    ),
                )
                .await?;
        }
        Ok(true)
    }

    /// 记录登录状态并在"失败→恢复"翻转时通知一次（`login_status` 存上一轮状态）。
    /// 故障期间静默重试不刷屏；恢复那一刻告诉你系统已自愈。成功时清零连续失败计数。
    async fn mark_login_state(&self, failed: bool) -> Result<()> {
        if !failed {
            // 清零必须带 revision 走 CAS：put_document(None) 是仅新建，
            // 计数文档已存在时写入会被静默丢弃，days 只增不减。
            let streak = self.store.document("jwxt_alert", "login_streak")?;
            self.store.put_document(
                "jwxt_alert",
                "login_streak",
                streak.as_ref().map(|d| d.revision),
                &serde_json::json!({ "days": 0, "last_date": "" }),
            )?;
        }
        let doc = self.store.document("jwxt_alert", "login_status")?;
        let was_failed = doc
            .as_ref()
            .map(|d| d.value["failed"].as_bool().unwrap_or(false))
            .unwrap_or(false);
        self.store.put_document(
            "jwxt_alert",
            "login_status",
            doc.as_ref().map(|d| d.revision),
            &serde_json::json!({ "failed": failed, "ts": crate::util::now() }),
        )?;
        if failed || !was_failed {
            return Ok(());
        }
        if let Some(notifier) = &self.notifier {
            notifier
                .send(
                    &self.config.notify_target,
                    None,
                    "✅ 教务登录已恢复：停机期间的课程提醒会自动补发",
                )
                .await?;
        }
        Ok(())
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

        let clock = self.config.clock.as_ref();
        let when = |m: &CourseMeeting| -> String {
            clock
                .and_then(|c| c.range_label(m.start_section, m.end_section))
                .unwrap_or_else(|| m.sections_raw.clone())
        };
        let old_sigs: HashSet<String> = old.meetings.iter().map(stored_signature).collect();
        let mut changes = Vec::new();
        for meeting in meetings {
            let sig = &meeting.signature();
            if !old_sigs.contains(sig) {
                changes.push(format!(
                    "**新增** {}｜{}周｜{}｜{}｜@{}",
                    meeting.course,
                    meeting.weeks_raw,
                    WEEKDAY_NAMES[(meeting.weekday as usize).clamp(1, 7) - 1],
                    when(meeting),
                    meeting.classroom
                ));
            }
        }
        let new_sigs: HashSet<String> = signatures.iter().cloned().collect();
        for meeting in &old.meetings {
            if !new_sigs.contains(stored_signature(meeting).as_str()) {
                let m = CourseMeeting {
                    course: meeting.course.clone(),
                    course_id: meeting.course_id.clone(),
                    teacher: meeting.teacher.clone(),
                    classroom: meeting.classroom.clone(),
                    weekday: meeting.weekday,
                    sections_raw: meeting.sections_raw.clone(),
                    start_section: meeting.start_section,
                    end_section: meeting.end_section,
                    weeks: meeting.weeks.clone(),
                    weeks_raw: meeting.weeks_raw.clone(),
                    class_type: meeting.class_type.clone(),
                    class_group: meeting.class_group.clone(),
                    xnm: String::new(),
                    xqm: String::new(),
                };
                changes.push(format!(
                    "**取消** {}｜{}周｜{}｜{}｜@{}",
                    meeting.course,
                    meeting.weeks_raw,
                    WEEKDAY_NAMES[(meeting.weekday as usize).clamp(1, 7) - 1],
                    when(&m),
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
            "## 📚 明日课表\n\n**{} {} · 第{}周**\n\n{}",
            tomorrow.format("%m月%d日"),
            weekday_name(tomorrow),
            week_of(start, tomorrow),
            if day_meetings.is_empty() {
                "明天没有课 🎉".to_owned()
            } else {
                let mut lines: Vec<String> = day_meetings
                    .iter()
                    .map(|m| StoredMeeting::from(m).display_line(self.config.clock.as_ref()))
                    .collect();
                lines.sort();
                format!("{}\n\n共 {} 节课", lines.join("\n"), lines.len())
            }
        );
        notifier
            .send(&self.config.notify_target, None, &message)
            .await?;
        Ok(true)
    }

    /// 日程同步：本周+下周的行课展开成一次性事件（kind=event，课前 20 分钟
    /// 由核心提醒循环推送、`/日程` 可见）。与已有 jwxt 事件按 source_id 求差：
    /// 缺的创建、多余的取消。幂等：无变化零写入；调课 = 取消旧 + 建新。
    async fn sync_schedule_events(
        &self,
        xnm: &str,
        xqm: &str,
        meetings: &[CourseMeeting],
        today: NaiveDate,
    ) -> Result<(usize, usize)> {
        use crate::app::schedule::ScheduleService;
        use crate::domain::schedule::{
            EVENT_KIND, EventInput, EventItem, EventRecurrence, EventSource,
        };
        use crate::domain::todo::TodoOptions;

        if !self.config.sync_schedule {
            return Ok((0, 0));
        }
        if self.config.semester_start.is_none() || self.config.clock.is_none() {
            // 一次性提示（不是每日）：缺 semester_start / section_times 时同步无法工作
            let key = "schedule_config";
            if self.store.document("jwxt_alert", key)?.is_none() {
                self.store.put_document(
                    "jwxt_alert",
                    key,
                    None,
                    &serde_json::json!({ "ts": crate::util::now() }),
                )?;
                if let Some(notifier) = &self.notifier {
                    notifier
                        .send(
                            &self.config.notify_target,
                            None,
                            "⚠️ 已开启课表日程同步，但缺少 semester_start 或 section_times（节次时刻表）配置，同步未生效",
                        )
                        .await?;
                }
            }
            return Ok((0, 0));
        }
        let semester_start = self.config.semester_start.unwrap();
        let clock = self.config.clock.clone().unwrap();
        let now = crate::util::now();

        // 目标事件集：本周 + 下周（周日晚已能看到下周一的课）
        let mut desired: std::collections::HashMap<String, EventInput> =
            std::collections::HashMap::new();
        for week in [
            week_of(semester_start, today),
            week_of(semester_start, today) + 1,
        ] {
            if week < 1 {
                continue;
            }
            let monday = semester_start + Duration::days((week - 1) * 7);
            for meeting in meetings.iter().filter(|m| m.weeks.contains(&(week as u32))) {
                let (Some(first), Some(last)) = (meeting.start_section, meeting.end_section) else {
                    continue;
                };
                let Some(start_time) = clock.times.get(first as usize - 1) else {
                    continue; // 节次超出时刻表范围，跳过（配置问题，不逐条告警）
                };
                let Some(end_time) = clock.times.get(last as usize - 1) else {
                    continue;
                };
                let day = monday + Duration::days(meeting.weekday as i64 - 1);
                let starts_at = chrono::Local
                    .from_local_datetime(&day.and_time(*start_time))
                    .single()
                    .map(|t| t.timestamp());
                let ends_at = chrono::Local
                    .from_local_datetime(
                        &day.and_time(*end_time + Duration::minutes(clock.duration_mins as i64)),
                    )
                    .single()
                    .map(|t| t.timestamp());
                let (Some(starts_at), Some(ends_at)) = (starts_at, ends_at) else {
                    continue;
                };
                let source_id = format!(
                    "{xnm}#{xqm}|{week}|{}|{}|{}-{}|{}|{}",
                    meeting.course_id,
                    meeting.weekday,
                    first,
                    last,
                    meeting.classroom,
                    meeting.teacher
                );
                desired.insert(
                    source_id.clone(),
                    EventInput {
                        title: meeting.course.clone(),
                        starts_at,
                        ends_at,
                        location: (!meeting.classroom.is_empty())
                            .then(|| meeting.classroom.clone()),
                        recurrence: EventRecurrence::None,
                        source: Some(EventSource {
                            channel: "jwxt".into(),
                            source_id: Some(source_id),
                        }),
                    },
                );
            }
        }

        // 已有 jwxt 事件（只看 active；cancelled 的不再复活）
        let service = ScheduleService::new(self.store.clone(), TodoOptions::default());
        let mut existing: std::collections::HashMap<String, EventItem> =
            std::collections::HashMap::new();
        for doc in self.store.documents(EVENT_KIND, 1000)? {
            let Ok(item) = serde_json::from_value::<EventItem>(doc.value) else {
                continue;
            };
            let Some(source) = &item.source else { continue };
            if source.channel != "jwxt"
                || item.status != crate::domain::schedule::EventStatus::Active
            {
                continue;
            }
            if let Some(source_id) = &source.source_id {
                existing.insert(source_id.clone(), item);
            }
        }

        let mut created = 0;
        let mut cancelled = 0;
        for (source_id, input) in &desired {
            match existing.get(source_id) {
                // 时间也要一致：section_times 配置调整后旧事件必须重建
                Some(item)
                    if item.starts_at == input.starts_at && item.ends_at == input.ends_at => {}
                Some(item) => {
                    service.cancel(&item.id, now)?;
                    service.add(input.clone(), now)?;
                    created += 1;
                    cancelled += 1;
                }
                None => {
                    service.add(input.clone(), now)?;
                    created += 1;
                }
            }
        }
        for (source_id, item) in &existing {
            if !desired.contains_key(source_id) {
                service.cancel(&item.id, now)?;
                cancelled += 1;
            }
        }
        Ok((created, cancelled))
    }

    pub async fn poll_once(&self) -> Result<JwxtPollReport> {
        let today = chrono::Local::now().date_naive();
        let (xnm, xqm) = resolve_term(
            self.config.xnm.as_deref(),
            self.config.xqm.as_deref(),
            today,
        );

        let term_key = format!("{xnm}#{xqm}");
        if let Err(err) = self.client.ensure_session().await {
            let alerted = self.alert_login_failure(&err, &term_key).await?;
            self.mark_login_state(true).await?;
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
                events_created: 0,
                events_cancelled: 0,
                daily_reminded: false,
                notified: alerted,
            });
        }

        let meetings = match self.client.timetable(&xnm, &xqm).await {
            Ok(meetings) => meetings,
            Err(err) => {
                let alerted = self.alert_login_failure(&err, &term_key).await?;
                self.mark_login_state(true).await?;
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
                    events_created: 0,
                    events_cancelled: 0,
                    daily_reminded: false,
                    notified: alerted,
                });
            }
        };

        let changes = self.diff_and_store(&xnm, &xqm, &meetings).await?;
        self.mark_login_state(false).await?;
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
        let (events_created, events_cancelled) = self
            .sync_schedule_events(&xnm, &xqm, &meetings, today)
            .await?;

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
            events_created,
            events_cancelled,
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
    clock: Option<&SectionClock>,
    date: NaiveDate,
) -> Result<String> {
    let (xnm, xqm) = resolve_term(None, None, chrono::Local::now().date_naive());
    let key = format!("{xnm}#{xqm}");
    let Some(doc) = store.document("jwxt_timetable", &key)? else {
        return Ok(
            "## 暂无课表缓存\n\n课表还没同步成功：`jwxt run` 未运行，或学校教务系统当前不可用（恢复后会自动同步）。"
                .to_owned(),
        );
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
        "## 📅 {} {} · 第{}周\n\n",
        date.format("%m月%d日"),
        weekday_name(date),
        week_of(start, date)
    );
    let body = if day_meetings.is_empty() {
        "没有课 🎉".to_owned()
    } else {
        let mut lines: Vec<String> = day_meetings
            .iter()
            .map(|m| StoredMeeting::from(m).display_line(clock))
            .collect();
        lines.sort();
        format!("{}\n\n共 {} 节课", lines.join("\n"), lines.len())
    };
    Ok(format!("{title}{body}"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::storage::TaskStore;
    use std::sync::Arc;

    fn test_tracker(store: Arc<dyn TaskStore>, dir: &std::path::Path) -> JwxtTracker {
        let client = crate::modules::jwxt::source::JwxtClient::new(
            "u",
            "p",
            "http://127.0.0.1:1".into(),
            "m".into(),
            "e".into(),
            "http://127.0.0.1:1".into(),
            dir.join("session.json"),
        );
        JwxtTracker::new(
            store,
            client,
            None,
            JwxtConfig {
                username: "u".into(),
                password: "p".into(),
                cas_base: "http://127.0.0.1:1".into(),
                cas_modulus: "m".into(),
                cas_exponent: "e".into(),
                jwxt_base: "http://127.0.0.1:1".into(),
                session_path: dir.join("session.json"),
                semester_start: None,
                xnm: None,
                xqm: None,
                interval_secs: 60,
                daily_remind: false,
                remind_hour: 21,
                sync_schedule: false,
                clock: None,
                notify_target: String::new(),
            },
        )
    }

    #[tokio::test]
    async fn login_streak_resets_after_recovery() {
        // 回归：put_document(None) 是仅新建，计数文档已存在时清零写入会被
        // 静默丢弃——恢复后 days 必须真的归零，否则恢复后再遇瞬时失败会立刻告警。
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            crate::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        // 模拟已连续失败 2 天（CAS 建的文档，revision 存在）
        store
            .put_document(
                "jwxt_alert",
                "login_streak",
                None,
                &serde_json::json!({ "days": 2, "last_date": "2026-10-24" }),
            )
            .unwrap();
        let tracker = test_tracker(store.clone(), dir.path());
        tracker.mark_login_state(false).await.unwrap();
        let doc = store
            .document("jwxt_alert", "login_streak")
            .unwrap()
            .expect("streak 文档在");
        assert_eq!(doc.value["days"].as_i64(), Some(0), "恢复后计数必须清零");
    }
}
