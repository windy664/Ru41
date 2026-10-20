use crate::{
    app::identity::IdentityService,
    ports::{
        commands::{CommandContext, CommandHandler, Reply},
        storage::TaskStore,
    },
};
use anyhow::Result;
use std::{future::Future, path::PathBuf, pin::Pin};

use super::super::zhihuishu::load_report;

/// 临期窗口（天）：与各平台推送提醒的 deadline_days 默认对齐。
const DUE_SOON_DAYS: i64 = 3;
/// 平台时间均为北京时间（UTC+8）。
const BEIJING_OFFSET: i32 = 28_800;

/// 跨平台 `/作业` 命令：学习通（records 快照）+ 智慧树（sidecar 报告）聚合成一张清单。
/// 权限语义与 ecard/zhihuishu 一致：先身份绑定，再限定单聊。
pub struct HomeworkQqCommand {
    report_path: PathBuf,
}

impl HomeworkQqCommand {
    pub fn new(report_path: PathBuf) -> Self {
        Self { report_path }
    }
}

impl CommandHandler for HomeworkQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(verb, "作业" | "homework" | "deadline")
    }

    fn help(&self) -> &'static str {
        "`/作业`：跨平台未完成作业清单（学习通 + 智慧树；需绑定身份，仅单聊）"
    }

    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            // 带参数不拦截：避免误吞自然语言，交给 Agent
            if !arg.is_empty() {
                return Ok(None);
            }
            if let Some(reply) = authorize(ctx)? {
                return Ok(Some(reply));
            }
            Ok(Some(Reply::text(render(ctx.store, &self.report_path))))
        })
    }

    fn handle_message<'a>(
        &'a self,
        content: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if !looks_like_homework_question(content) {
                return Ok(None);
            }
            if let Some(reply) = authorize(ctx)? {
                return Ok(Some(reply));
            }
            Ok(Some(Reply::text(render(ctx.store, &self.report_path))))
        })
    }
}

/// 与 ecard/zhihuishu 同款双闸：先身份绑定，再限定单聊（作业属个人数据）。
fn authorize(ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
    let identity = IdentityService::new(ctx.store.clone());
    if !identity.is_bound("qq", ctx.author_openid)? {
        return Ok(Some(Reply::text(
            "## 需要身份绑定\n\n请先发送 `绑定 <口令>`，再查询作业。".to_owned(),
        )));
    }
    if !ctx.is_c2c {
        return Ok(Some(Reply::text(
            "## 请单聊查询\n\n作业信息属于个人数据，群聊不展示。".to_owned(),
        )));
    }
    Ok(None)
}

/// 自然语言命中要克制：既提「作业/截止」又带疑问色彩才拦截，避免日常闲聊被吞。
fn looks_like_homework_question(content: &str) -> bool {
    let text = content.trim().to_lowercase();
    let topic = ["作业", "截止", "deadline", "due"]
        .iter()
        .any(|k| text.contains(k));
    let asking = ["什么", "哪些", "还有", "有没有", "查", "快", "？", "?"]
        .iter()
        .any(|k| text.contains(k));
    topic && asking
}

/// 一条作业行：平台、课程、标题、截止时间戳、是否考试式。
struct Row {
    platform: &'static str,
    course: String,
    title: String,
    end_ts: Option<i64>,
    exam_style: bool,
}

/// 聚合渲染（纯函数，可测）：未交按截止升序分桶，完成只计数。
fn render(store: &dyn TaskStore, report_path: &std::path::Path) -> String {
    let now = crate::util::now();
    let (open, done) = collect_rows(store, report_path);
    if open.is_empty() && done == 0 {
        return "## 暂无作业数据\n\n两个平台都还没有采集到作业；等下一轮轮询后再问吧。".to_owned();
    }
    if open.is_empty() {
        return format!("## 作业清单\n\n当前没有未完成作业 🎉（已完成 {done} 条）");
    }

    let urgent = DUE_SOON_DAYS * 86_400;
    let mut overdue: Vec<&Row> = Vec::new();
    let mut due_soon: Vec<&Row> = Vec::new();
    let mut later: Vec<&Row> = Vec::new();
    for row in &open {
        match row.end_ts {
            Some(ts) if ts < now => overdue.push(row),
            Some(ts) if ts - now <= urgent => due_soon.push(row),
            _ => later.push(row),
        }
    }
    // 各桶内按截止升序；无截止的垫底
    let by_end = |a: &&Row, b: &&Row| {
        a.end_ts
            .unwrap_or(i64::MAX)
            .cmp(&b.end_ts.unwrap_or(i64::MAX))
    };
    overdue.sort_by(by_end);
    due_soon.sort_by(by_end);
    later.sort_by(by_end);

    let mut out = String::from("## 作业清单\n\n");
    let push_row = |out: &mut String, row: &Row| {
        let exam = if row.exam_style { " 🔒考试式" } else { "" };
        let end = row
            .end_ts
            .map(|ts| {
                let remain = ts - now;
                let tail = if remain < 0 {
                    "已过期".to_owned()
                } else if remain < 86_400 {
                    format!("剩 {} 小时", remain.div_euclid(3600).max(1))
                } else {
                    format!("剩 {} 天", remain.div_euclid(86_400))
                };
                format!("{}（{tail}）", fmt_mmdd_hhmm(ts))
            })
            .unwrap_or_else(|| "未知".to_owned());
        out.push_str(&format!(
            "- {}｜{}｜{}{} 📅 {end}\n",
            row.platform, row.course, row.title, exam
        ));
    };
    if !overdue.is_empty() {
        out.push_str(&format!("### ⚠️ 已过期（{}）\n\n", overdue.len()));
        for row in &overdue {
            push_row(&mut out, row);
        }
        out.push('\n');
    }
    if !due_soon.is_empty() {
        out.push_str(&format!(
            "### ⏰ {DUE_SOON_DAYS} 天内截止（{}）\n\n",
            due_soon.len()
        ));
        for row in &due_soon {
            push_row(&mut out, row);
        }
        out.push('\n');
    }
    if !later.is_empty() {
        out.push_str(&format!("### 📝 其他未交（{}）\n\n", later.len()));
        for row in &later {
            push_row(&mut out, row);
        }
        out.push('\n');
    }
    out.push_str(&format!("—— 已完成 {done} 条；清单见 Obsidian「作业总表」"));
    out
}

/// 收集两平台的开放/完成行：新平台接入时在此追加。
fn collect_rows(store: &dyn TaskStore, report_path: &std::path::Path) -> (Vec<Row>, usize) {
    let mut open = Vec::new();
    let mut done = 0usize;

    // 学习通：records 快照（chaoxing-work 轮询写入）
    if let Ok(docs) = store.documents("chaoxing_work", 500) {
        for doc in docs {
            let value = &doc.value;
            let status = value
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let row = || Row {
                platform: "学习通",
                course: field(value, "course"),
                title: field(value, "title"),
                end_ts: value
                    .get("end_at")
                    .and_then(serde_json::Value::as_str)
                    .and_then(parse_beijing),
                exam_style: false,
            };
            if status.contains('未') {
                open.push(row());
            } else {
                done += 1;
            }
        }
    }

    // 智慧树：sidecar 报告（同 zhihuishu 命令的数据源，保持新鲜度一致）
    if let Ok(report) = load_report(report_path) {
        for course in &report.courses {
            for task in &course.tasks {
                let exam_style = task.exam_duration_minutes.unwrap_or(0) > 0
                    || task.video_invigilate == Some(true);
                let row = Row {
                    platform: "智慧树",
                    course: course.title.clone(),
                    title: task.title.clone(),
                    end_ts: task
                        .end_at
                        .as_deref()
                        .and_then(crate::modules::zhihuishu::parse_platform_time),
                    exam_style,
                };
                if task.completed.unwrap_or(false) {
                    done += 1;
                } else {
                    open.push(row);
                }
            }
        }
    }
    (open, done)
}

fn field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// 平台时间串（北京时间）→ 时间戳；兼容带/不带秒两种格式。
fn parse_beijing(raw: &str) -> Option<i64> {
    use chrono::TimeZone;
    let raw = raw.trim();
    let naive = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M"))
        .ok()?;
    chrono::FixedOffset::east_opt(BEIJING_OFFSET)?
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.timestamp())
}

/// 时间戳 → "MM-DD HH:MM"（北京时间）。
fn fmt_mmdd_hhmm(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::FixedOffset::east_opt(BEIJING_OFFSET)
        .and_then(|tz| tz.timestamp_opt(ts, 0).single())
        .map(|dt| dt.format("%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seed_store() -> crate::adapters::database::sqlite::SqliteTaskStore {
        let dir = std::env::temp_dir().join(format!("ru41-hw-test-{}", crate::util::now()));
        let store = crate::adapters::database::sqlite::SqliteTaskStore::open(&dir).unwrap();
        // 学习通：一条未交（临期）+ 一条已交
        store
            .put_document(
                "chaoxing_work",
                "1#9001",
                None,
                &json!({
                    "course": "大学英语新视野",
                    "title": "作业一：函数极限",
                    "status": "未交",
                    "end_at": beijing_days_from_now(1),
                }),
            )
            .unwrap();
        store
            .put_document(
                "chaoxing_work",
                "1#9002",
                None,
                &json!({"course": "大学英语新视野", "title": "作业二", "status": "已提交"}),
            )
            .unwrap();
        store
    }

    /// N 天后的北京时间 "YYYY-MM-DD HH:MM"（相对当前时刻平移）。
    fn beijing_days_from_now(days: i64) -> String {
        use chrono::TimeZone;
        chrono::FixedOffset::east_opt(BEIJING_OFFSET)
            .unwrap()
            .timestamp_opt(crate::util::now() + days * 86_400, 0)
            .single()
            .unwrap()
            .format("%Y-%m-%d %H:%M")
            .to_string()
    }

    fn sample_report(path: &std::path::Path) {
        let report = json!({
            "generated_at": "2026-10-20 12:00:00",
            "login_ok": true,
            "courses": [{
                "title": "职业生涯规划",
                "tasks": [
                    {"activity_task_id": "T1", "title": "第五节课 课前测",
                     "end_at": "2099-01-01 00:00:00", "completed": false},
                    {"activity_task_id": "T2", "title": "期末考模拟",
                     "end_at": "2099-06-01 00:00:00", "completed": true,
                     "exam_duration_minutes": 30},
                ]
            }]
        });
        std::fs::write(path, serde_json::to_string(&report).unwrap()).unwrap();
    }

    #[test]
    fn render_aggregates_both_platforms() {
        let store = seed_store();
        let dir = std::env::temp_dir().join(format!("ru41-hw-rpt-{}", crate::util::now()));
        std::fs::create_dir_all(&dir).unwrap();
        let report_path = dir.join("report.json");
        sample_report(&report_path);

        let text = render(&store, &report_path);
        // 两个平台同屏
        assert!(text.contains("学习通｜大学英语新视野｜作业一：函数极限"));
        assert!(text.contains("智慧树｜职业生涯规划｜第五节课 课前测"));
        // 分桶：学习通临期在「3 天内截止」，智慧树远期在「其他未交」
        assert!(text.contains("### ⏰ 3 天内截止（1）"));
        assert!(text.contains("### 📝 其他未交（1）"));
        // 完成计数跨平台合并
        assert!(text.contains("已完成 2 条"));
        assert!(text.contains("作业总表"));
    }

    #[test]
    fn render_empty_is_friendly() {
        let dir = std::env::temp_dir().join(format!("ru41-hw-empty-{}", crate::util::now()));
        std::fs::create_dir_all(&dir).unwrap();
        let store_path = dir.join("s.db");
        let store = crate::adapters::database::sqlite::SqliteTaskStore::open(&store_path).unwrap();
        let text = render(&store, &dir.join("missing.json"));
        assert!(text.contains("暂无作业数据"));
    }

    #[test]
    fn parse_beijing_accepts_both_shapes() {
        let a = parse_beijing("2026-11-03 23:59").unwrap();
        let b = parse_beijing("2026-11-03 23:59:00").unwrap();
        assert_eq!(a, b);
        assert!(parse_beijing("垃圾").is_none());
    }
}
