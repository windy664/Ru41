//! jwxt 模块的 CLI 处理：课表缓存查询与 CAS 两步登录。
//! 逻辑属于附属本身；main.rs 只做参数路由，不再认识 jwxt 内部。

use crate::{modules::jwxt::JwxtTracker, ports::storage::TaskStore};
use anyhow::{Context, Result};
use std::sync::Arc;

use super::{JwxtSection, describe_day_from_store};

/// 读本地课表缓存（jwxt_timetable 快照）回答某天/本周的课。
/// 只读缓存，不触网、不需要登录态，供 agent skill 与排障使用。
pub fn classes_json(
    store: Arc<dyn TaskStore>,
    section: &JwxtSection,
    when: &str,
) -> Result<serde_json::Value> {
    use chrono::Datelike;
    let semester_start = section
        .semester_start
        .as_deref()
        .and_then(|raw| chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").ok());
    let clock = section.section_clock()?;
    let today = chrono::Local::now().date_naive();
    let when = when.trim();
    if when == "week" || when.contains('周') {
        let mut lines = Vec::new();
        for offset in 0..(7 - today.weekday().num_days_from_monday() as i64) {
            let day = today + chrono::Duration::days(offset);
            let text = describe_day_from_store(&store, semester_start, clock.as_ref(), day)?;
            let body = text.split_once("\n\n").map(|(_, b)| b).unwrap_or(&text);
            lines.push(format!("**{}**\n{}", day.format("%m月%d日"), body));
        }
        return Ok(format!("## 本周课表\n\n{}", lines.join("\n\n")).into());
    }
    let date = match when {
        "" | "today" | "今天" => today,
        "tomorrow" | "明天" => today + chrono::Duration::days(1),
        raw => chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").with_context(|| {
            format!("无法识别的时间：{raw}（支持 today/tomorrow/week/YYYY-MM-DD）")
        })?,
    };
    Ok(describe_day_from_store(&store, semester_start, clock.as_ref(), date)?.into())
}

/// CAS 验证码登录的两步交互：无 captcha 时下载验证码图片并提示路径；
/// 带 captcha 时取持久化的 uid 提交答案，成功后 TGT 已入会话文件。
pub async fn login_json(tracker: &JwxtTracker, captcha: Option<&str>) -> Result<serde_json::Value> {
    let client = tracker.client();
    match captcha {
        Some(code) => {
            let uid = client
                .pending_captcha_uid()
                .context("没有待验证的验证码：请先运行 `ru41 jwxt login` 下载验证码图片")?;
            client.cas_login(&uid, code.trim()).await?;
            Ok(serde_json::json!({"login": "ok"}))
        }
        None => {
            let (uid, path) = client.download_captcha().await?;
            Ok(serde_json::json!({
                "message": "查看验证码图片后运行 `ru41 jwxt login --captcha <答案>`",
                "uid": uid,
                "image": path.display().to_string(),
            }))
        }
    }
}
