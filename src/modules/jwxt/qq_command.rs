use crate::ports::commands::{CommandContext, CommandHandler, Reply};
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate};

use super::service::SectionClock;
use std::future::Future;
use std::pin::Pin;

/// QQ 课表查询命令；读 jwxt_timetable 缓存快照，不触网、不做登录。
pub struct JwxtQqCommand {
    /// 学期第一周周一（来自 [sources.jwxt]），周次过滤依赖。
    semester_start: Option<NaiveDate>,
    /// 节次时刻表：把节次换算成具体钟点显示；未配置则回退节次文本。
    clock: Option<SectionClock>,
}

impl JwxtQqCommand {
    pub fn new(semester_start: Option<NaiveDate>, clock: Option<SectionClock>) -> Self {
        Self {
            semester_start,
            clock,
        }
    }
}

impl CommandHandler for JwxtQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(verb, "课表" | "明日课表" | "课程表" | "schedule")
    }

    fn help(&self) -> &'static str {
        "`/课表 [明天|周]`：查今天/明天/本周课表（需绑定身份，仅单聊）；也可问“明天有什么课”"
    }

    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move { self.respond("课表", arg, ctx).await })
    }

    /// 区分动词：/明日课表 即使不带参数也查明天。
    fn handle_command<'a>(
        &'a self,
        verb: &'a str,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move { self.respond(verb, arg, ctx).await })
    }

    fn handle_message<'a>(
        &'a self,
        content: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            let Some(date) = looks_like_timetable_question(content) else {
                return Ok(None);
            };
            if let Some(reply) = ctx.require_private("查询课表", "课表属于个人信息")? {
                return Ok(Some(reply.into()));
            }
            Ok(Some(
                super::service::describe_day_from_store(
                    ctx.store,
                    self.semester_start,
                    self.clock.as_ref(),
                    date,
                )?
                .into(),
            ))
        })
    }
}

impl JwxtQqCommand {
    async fn respond<'a>(
        &'a self,
        verb: &'a str,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Result<Option<Reply>> {
        let arg = arg.trim();
        // 带参数的用法只认 明天/tomorrow/周/week，其余不拦截（避免误吞自然语言）
        if !arg.is_empty()
            && !(arg.contains("明")
                || arg.contains("tomorrow")
                || arg.contains("周")
                || arg.contains("week"))
        {
            return Ok(None);
        }
        if let Some(reply) = ctx.require_private("查询课表", "课表属于个人信息")? {
            return Ok(Some(reply.into()));
        }
        let today = chrono::Local::now().date_naive();
        let date = if verb.contains("明") || arg.contains("明") || arg.contains("tomorrow") {
            today + Duration::days(1)
        } else if arg.contains("周") || arg.contains("week") {
            // /课表 周：本周视图从今天开始逐日列出
            let mut lines = Vec::new();
            for offset in 0..(7 - today.weekday().num_days_from_monday() as i64) {
                let day = today + Duration::days(offset);
                let text = super::service::describe_day_from_store(
                    ctx.store,
                    self.semester_start,
                    self.clock.as_ref(),
                    day,
                )?;
                lines.push(format!(
                    "**{}**\n{}",
                    day.format("%m月%d日"),
                    day_body(&text)
                ));
            }
            return Ok(Some(
                format!("## 本周课表\n\n{}", lines.join("\n\n")).into(),
            ));
        } else {
            today
        };
        Ok(Some(
            super::service::describe_day_from_store(
                ctx.store,
                self.semester_start,
                self.clock.as_ref(),
                date,
            )?
            .into(),
        ))
    }
}

/// describe_day 的输出形如 `## 10月21日（第3周 星期2）\n\n正文`，取正文部分。
fn day_body(describe_output: &str) -> &str {
    describe_output
        .split_once("\n\n")
        .map(|(_, body)| body)
        .unwrap_or(describe_output)
}

/// 自然语言："明天有什么课" / "今天课多吗" / "周三有什么课"。
/// 返回 Some(目标日期) 表示拦截；保守匹配，只认「课」+ 明确时间词。
/// 转发的通知/公告常带「课」（如“第二课堂学分”）和「今天/周X」（如“10月27日（今天）”）
/// 字样，长文本或含通知特征词的一律不拦截，交给下游 notices 抽取或 agent。
fn looks_like_timetable_question(content: &str) -> Option<NaiveDate> {
    let text = content.trim();
    if text.chars().count() > TIMETABLE_QUESTION_MAX_CHARS || looks_like_notice(text) {
        return None;
    }
    if !text.contains('课') {
        return None;
    }
    let today = chrono::Local::now().date_naive();
    if text.contains("明天") || text.contains("明日") {
        return Some(today + Duration::days(1));
    }
    if text.contains("今天") || text.contains("今日") {
        return Some(today);
    }
    for (name, weekday) in [
        ("周一", 1),
        ("星期一", 1),
        ("周二", 2),
        ("星期二", 2),
        ("周三", 3),
        ("星期三", 3),
        ("周四", 4),
        ("星期四", 4),
        ("周五", 5),
        ("星期五", 5),
        ("周六", 6),
        ("星期六", 6),
        ("周日", 7),
        ("星期日", 7),
        ("星期天", 7),
    ] {
        if text.contains(name) {
            let today_no = today.weekday().number_from_monday() as i64;
            let delta = (weekday as i64 - today_no).rem_euclid(7);
            return Some(today + Duration::days(if delta == 0 { 7 } else { delta }));
        }
    }
    None
}

/// 课表提问的长度闸：真实提问都是短句（“明天有什么课”7 字），
/// 转发的通知动辄上百字，超过阈值直接不拦。
const TIMETABLE_QUESTION_MAX_CHARS: usize = 40;

/// 通知/公告特征词：命中任意一个就不当作课表提问。
fn looks_like_notice(text: &str) -> bool {
    [
        "通知", "宣讲", "讲座", "班会", "大会", "参会", "座位", "报名", "签到", "地点",
        "时间：", "时间:", "学分", "请同学", "请假", "主讲", "会议要求",
    ]
    .iter()
    .any(|k| text.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_questions_still_match() {
        let today = chrono::Local::now().date_naive();
        assert_eq!(
            looks_like_timetable_question("明天有什么课"),
            Some(today + Duration::days(1))
        );
        assert_eq!(looks_like_timetable_question("今天课多吗"), Some(today));
    }

    #[test]
    fn notice_with_ke_and_today_is_not_a_question() {
        // 真实案例：表彰大会通知同时含「第二课堂学分」和「（今天）」，
        // 曾经被拦成“今天课表”回复，导致通知没进行程。
        let notice = "📣通知：为树立优秀学习模范，我院将于10月27日举行学风表彰大会暨优秀学子宣讲会。\n\
                      ⏰时间：10月27日（今天）下午13:30-15:00\n\
                      🏠地点：学术报告厅\n\
                      ⚠️该会议后面可以算第二课堂学分，新生班级有事可以跟学习委员请假";
        assert_eq!(looks_like_timetable_question(notice), None);
    }

    #[test]
    fn long_text_is_not_a_question() {
        let long = "老师布置了下周的阅读任务，一共三章内容，另外星期二有几门课要记得提前预习好，别忘记了".repeat(2);
        assert!(long.chars().count() > TIMETABLE_QUESTION_MAX_CHARS);
        assert_eq!(looks_like_timetable_question(&long), None);
    }

    #[test]
    fn non_questions_still_do_not_match() {
        assert_eq!(looks_like_timetable_question("帮我订个外卖"), None);
    }
}
