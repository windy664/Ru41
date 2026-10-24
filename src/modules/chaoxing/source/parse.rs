//! 学习通页面解析：作业列表/截止时间（HTML）与课程列表（HTML）。
//! 页面改版集中体现在这些函数。

use super::model::{ChaoxingCourse, ChaoxingWork};

pub(super) fn parse_work_list(html: &str) -> Vec<ChaoxingWork> {
    let re_li =
        regex::Regex::new(r#"(?s)<li[^>]*data="([^"]+)"[^>]*>(.*?)</li>"#).expect("li regex");
    let re_title = regex::Regex::new(r#"class="overHidden2[^"]*">([^<]+)<"#).expect("title regex");
    let re_status = regex::Regex::new(r#"class="status[^"]*">([^<]+)<"#).expect("status regex");
    let re_id = regex::Regex::new(r"[?&]workId=(\d+)").expect("workId regex");
    let mut out = Vec::new();
    for cap in re_li.captures_iter(html) {
        let (url, body) = (&cap[1], &cap[2]);
        let Some(work_id) = re_id.captures(url).map(|c| c[1].to_owned()) else {
            continue;
        };
        out.push(ChaoxingWork {
            work_id,
            title: re_title
                .captures(body)
                .map(|c| c[1].trim().to_owned())
                .unwrap_or_else(|| "未命名作业".into()),
            status: re_status
                .captures(body)
                .map(|c| c[1].trim().to_owned())
                .unwrap_or_else(|| "未知".into()),
            detail_url: url.replace("&amp;", "&"),
        });
    }
    out
}

/// 从详情页提取截止时间，归一化为 "YYYY-MM-DD HH:MM"。
/// 平台渲染「作答时间: MM-DD HH:MM 至 MM-DD HH:MM」（缺年份）；
/// 跨年按「距今不超过 180 天的最近未来」补年。
pub(super) fn parse_work_deadline(html: &str, now: i64) -> Option<String> {
    // 全格式（含年份）
    if let Some(end) = capture(
        html,
        r"作答时间[^<]*?\d{4}-\d{2}-\d{2} \d{2}:\d{2}[^<]*?至[^<]*?(\d{4}-\d{2}-\d{2} \d{2}:\d{2})",
    ) {
        return Some(end);
    }
    // 缺年份变体：按「距今不超过 180 天的最近合理年份」补全
    let end = capture(
        html,
        r"作答时间[^<]*?\d{2}-\d{2} \d{2}:\d{2}[^<]*?至[^<]*?(\d{2}-\d{2} \d{2}:\d{2})",
    )?;
    let year = infer_deadline_year(now, &end)?;
    Some(format!("{year}-{end}"))
}

/// 把 "MM-DD HH:MM" 补全到距今 180 天内的最近年份。
fn infer_deadline_year(now: i64, end_mmdd: &str) -> Option<i32> {
    use chrono::Datelike;
    let today = chrono::DateTime::from_timestamp(now, 0)?.date_naive();
    let current = today.year();
    for year in [current, current + 1, current - 1] {
        // end_mmdd 形如 "MM-DD HH:MM"，只取前 5 位日期
        let Ok(date) =
            chrono::NaiveDate::parse_from_str(&format!("{year}-{}", &end_mmdd[..5]), "%Y-%m-%d")
        else {
            continue;
        };
        if (-180..=180).contains(&(date - today).num_days()) {
            return Some(year);
        }
    }
    None
}

pub(super) fn decode_course_list(html: &str) -> Vec<ChaoxingCourse> {
    let re_div = regex::Regex::new(r#"<div class="course(?: [^"]*)?""#).expect("course div regex");
    let mut courses = Vec::new();
    for chunk in re_div.split(html).skip(1) {
        if chunk.contains("not-open-tip") {
            continue;
        }
        let Some(course_id) = capture(chunk, r#"name="courseId"\s+value="(\d+)""#) else {
            continue;
        };
        let Some(clazz_id) = capture(chunk, r#"name="clazzId"\s+value="(\d+)""#) else {
            continue;
        };
        let Some(cpi) = capture(chunk, r#"cpi=(\d+)&"#) else {
            continue;
        };
        let Some(title) = capture(chunk, r#"class="course-name[^"]*"[^>]*title="([^"]+)""#) else {
            continue;
        };
        let teacher =
            capture(chunk, r#"class="[^"]*color3[^"]*"[^>]*title="([^"]*)""#).unwrap_or_default();
        courses.push(ChaoxingCourse {
            course_id,
            clazz_id,
            cpi,
            title,
            teacher,
        });
    }
    courses
}

pub(super) fn capture(text: &str, pattern: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).ok()?;
    re.captures(text)?.get(1).map(|m| m.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::super::model::ChaoxingActivity;
    use super::*;

    #[test]
    fn decode_course_list_skips_not_open() {
        let html = r#"<div class="course" id="c1">
            <input type="hidden" name="clazzId" value="111" class="clazzId">
            <input type="hidden" name="courseId" value="222" class="courseId">
            <a href="https://x.chaoxing.com/learn?cpi=333&x=1"></a>
            <span class="course-name" title="高等数学I(理)">高等数学I(理)</span>
            <p class="color3" title="张老师">张老师</p>
        </div>
        <div class="course" id="c2">
            <a class="not-open-tip"></a>
            <input type="hidden" name="clazzId" value="444" class="clazzId">
            <input type="hidden" name="courseId" value="555" class="courseId">
            <a href="https://x.chaoxing.com/learn?cpi=666&x=1"></a>
            <span class="course-name" title="未开放课">未开放课</span>
        </div>"#;
        let courses = decode_course_list(html);
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].course_id, "222");
        assert_eq!(courses[0].clazz_id, "111");
        assert_eq!(courses[0].cpi, "333");
        assert_eq!(courses[0].title, "高等数学I(理)");
        assert_eq!(courses[0].teacher, "张老师");
    }

    #[test]
    fn parse_activity_json() {
        let raw = r#"{"userStatus":1,"nameOne":"二维码签到","attendNum":57,
            "activeType":2,"type":2,"startTime":1792135761000,"id":1000174332257,
            "endTime":1792135881000,"status":2,"unknown_field":true}"#;
        let a: ChaoxingActivity = serde_json::from_str(raw).unwrap();
        assert_eq!(a.id, 1000174332257);
        assert!(a.is_sign());
        assert!(!a.is_pending());
        assert_eq!(a.type_label(), "二维码签到");
    }
}
