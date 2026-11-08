//! 学习通页面解析：作业列表/截止时间（HTML）与课程列表（HTML）。
//! 页面改版集中体现在这些函数。

use super::model::{ChaoxingCourse, ChaoxingWork, WorkContent};

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

/// 解析作业详情页里的题干：文字 + 图片。
///
/// 题干在 `<h3 class="mark_name">` 里（平台渲染的题目块）；个别题型落在
/// `<div class="mark_content">`。图片多是平台 CDN（p.ananas.chaoxing.com），
/// 也可能写协议相对的 `//host/path`。
pub(super) fn parse_work_content(html: &str) -> WorkContent {
    let re_stem = regex::Regex::new(r#"(?s)<h3[^>]*class="[^"]*mark_name[^"]*"[^>]*>(.*?)</h3>"#)
        .expect("stem regex");
    let re_block =
        regex::Regex::new(r#"(?s)<div[^>]*class="[^"]*mark_content[^"]*"[^>]*>(.*?)</div>"#)
            .expect("mark_content regex");
    let re_img = regex::Regex::new(r#"(?s)<img[^>]*src="([^"]+)""#).expect("img regex");

    let mut texts: Vec<String> = Vec::new();
    let mut images: Vec<String> = Vec::new();
    let collect = |stem: &str, texts: &mut Vec<String>, images: &mut Vec<String>| {
        let text = html_to_text(stem);
        if !text.is_empty() {
            texts.push(text);
        }
        for cap in re_img.captures_iter(stem) {
            if let Some(url) = normalize_image_url(&cap[1])
                && !images.contains(&url)
            {
                images.push(url);
            }
        }
    };

    for cap in re_stem.captures_iter(html) {
        collect(&cap[1], &mut texts, &mut images);
    }
    if texts.is_empty() && images.is_empty() {
        for cap in re_block.captures_iter(html) {
            collect(&cap[1], &mut texts, &mut images);
        }
    }
    WorkContent {
        text: texts.join("\n\n"),
        images,
    }
}

/// 去掉所有标签。
fn re_tags() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r#"(?s)<[^>]*>"#).expect("tag regex"))
}

/// HTML 片段转纯文本：块级标签换行，其余去标签，最后压空白。
fn html_to_text(html: &str) -> String {
    let with_breaks = html
        .replace("<br", "\n<br")
        .replace("<br/>", "\n")
        .replace("</p>", "\n")
        .replace("</div>", "\n")
        .replace("</li>", "\n");
    let stripped = re_tags().replace_all(&with_breaks, "");
    let decoded = decode_entities(&stripped);
    let mut lines: Vec<String> = decoded
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    lines.dedup();
    lines.join("\n")
}

/// 图片地址归一：协议相对补 https，站内绝对路径补平台域，其余丢弃。
fn normalize_image_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(rest) = raw.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    if raw.starts_with("https://") {
        return Some(raw.to_owned());
    }
    if raw.starts_with("http://") {
        return Some(raw.replacen("http://", "https://", 1));
    }
    None
}

/// 常见 HTML 实体解码（作业描述里够用）。
fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// 从详情页提取截止时间，归一化为 "YYYY-MM-DD HH:MM"。
/// 平台渲染「作答时间: MM-DD HH:MM 至 MM-DD HH:MM」（缺年份），且日期常裹在
/// `<em>` 等内联标签里（2026-11-08 实测「描述打扫卫生」作业，标签让旧正则
/// 全程失配、截止时间一直是 null、临期提醒从未发出）——先剥标签再匹配。
/// 跨年按「距今不超过 180 天的最近未来」补年。
pub(super) fn parse_work_deadline(html: &str, now: i64) -> Option<String> {
    let plain = re_tags().replace_all(html, "");
    // 全格式（含年份）
    if let Some(end) = capture(
        &plain,
        r"(?s)作答时间.{0,40}?\d{4}-\d{2}-\d{2} \d{2}:\d{2}.{0,20}?至.{0,20}?(\d{4}-\d{2}-\d{2} \d{2}:\d{2})",
    ) {
        return Some(end);
    }
    // 缺年份变体：按「距今不超过 180 天的最近合理年份」补全
    let end = capture(
        &plain,
        r"(?s)作答时间.{0,40}?\d{2}-\d{2} \d{2}:\d{2}.{0,20}?至.{0,20}?(\d{2}-\d{2} \d{2}:\d{2})",
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
    use super::*;
    use crate::source::model::ChaoxingActivity;

    /// 平台详情页的日期裹在 <em> 等内联标签里（2026-11-08 实测），剥标签后也要解析得出。
    #[test]
    fn parse_work_deadline_tolerates_inline_tags() {
        let html = r#"<p>作答时间:<em>11-06 16:42</em>至<em>11-08 17:42</em></p>"#;
        let now = 1_793_563_170; // 2026-11-05 晚间附近
        let deadline = parse_work_deadline(html, now).expect("应解析出截止时间");
        assert_eq!(deadline, "2026-11-08 17:42");
    }

    /// 题干可能只有一张图（讲卫生那类作业），文字与图片都要抓到。
    #[test]
    fn parse_work_content_keeps_text_and_images() {
        let html = r#"
            <h3 class="mark_name colorDeep fontLabel workTextWrap">1.
                <span class="colorShallow">(简答题)</span>
                <p>请描述宿舍卫生打扫情况<br>要求 300 字以内。</p>
                <p><img class="ans-ued-img" src="https://p.ananas.chaoxing.com/star3/750_1024/abc.jpg"></p>
            </h3>
            <img src="//mooc1.chaoxing.com/mooc-ans/mooc2/images/popClose.png">
        "#;
        let content = parse_work_content(html);
        assert!(content.text.contains("请描述宿舍卫生打扫情况"));
        assert!(content.text.contains("要求 300 字以内"));
        assert_eq!(
            content.images,
            vec!["https://p.ananas.chaoxing.com/star3/750_1024/abc.jpg".to_owned()]
        );
    }

    /// 纯图片题干：没有文字也要把图片带出来。
    #[test]
    fn parse_work_content_handles_image_only_stem() {
        let html = r#"
            <h3 class="mark_name colorDeep">1.
                <p><img class="ans-ued-img" src="//p.ananas.chaoxing.com/star3/x.jpg"></p>
            </h3>
        "#;
        let content = parse_work_content(html);
        // 题号还在（"1."），但没有实质文字——图片才是内容。
        assert!(!content.text.contains("描述"));
        assert_eq!(
            content.images,
            vec!["https://p.ananas.chaoxing.com/star3/x.jpg".to_owned()]
        );
    }

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
