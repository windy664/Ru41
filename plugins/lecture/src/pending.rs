//! 待转写队列：handler 下载落盘后写入 `lecture.pending` 文档，
//! worker 逐条消费（切片转写 → 内联 prompt 入队 agent 整理笔记 → 清理工作区）。
//! 状态机：pending → done；failed 终态不自动重试（错误会通过 report 暴露，
//! 人工排查后删文档重投）。vault 只沉淀笔记，转写与音频均为临时产物。

use anyhow::{Context, Result};
use ru41::domain::records::Document;
use serde::{Deserialize, Serialize};

pub const PENDING_KIND: &str = "lecture.pending";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingItem {
    /// 已下载到本地的原始录音绝对路径（QQ CDN 有时效，handler 必须先落盘）。
    pub file: String,
    /// 原始文件名（展示与 vault 命名用）。
    pub filename: String,
    pub size: u64,
    pub channel: String,
    pub target: String,
    pub msg_id: String,
    /// 收到录音的 unix 时间戳（秒）；课程匹配与归档日期都基于它。
    pub received_at: i64,
    /// pending | transcribed | done | failed
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub course: Option<String>,
}

fn default_status() -> String {
    "pending".into()
}

impl PendingItem {
    pub fn from_document(doc: &Document) -> Result<Self> {
        serde_json::from_value(doc.value.clone())
            .with_context(|| format!("lecture.pending 文档 {} 解析失败", doc.id))
    }

    /// 课程匹配与归档日期用的时间：优先文件名里解析出的开录时刻
    /// （录音 app 命名通常带，比收到时刻更贴近实际上课时间），解析不出回退收到时间。
    pub fn recorded_time(&self) -> i64 {
        parse_recorded_at(&self.filename).unwrap_or(self.received_at)
    }

    /// 录音时间来源说明（提示词里向 agent 交代依据）。
    pub fn recorded_time_source(&self) -> &'static str {
        if parse_recorded_at(&self.filename).is_some() {
            "来自录音文件名"
        } else {
            "文件名无时间，按收到时刻回退"
        }
    }
}

/// 从录音文件名解析开录时刻（本地时区 unix 秒）。覆盖常见录音 app 命名：
/// `20261103_143000`、`20261103143000`、`2026-11-03 14-30-05`、
/// `Rec_2026.11.03-14.30`、`录音 2026年11月3日 14_30`、
/// `2026年11月04日 上午09点11分(1).mp3`（QQ 常见中文时刻格式）。
/// 解析不出返回 None。
pub fn parse_recorded_at(filename: &str) -> Option<i64> {
    use chrono::{Local, NaiveDate, TimeZone};
    // 去扩展名；时间串可能在任意位置（Rec_ 前缀、「录音 」等字样前后）。
    let stem = filename
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(filename);
    // 分隔符格式：2026-11-03 14-30-05 / 2026.11.03-14.30 / 2026年11月3日 14_30
    // / 2026年11月4日 下午05点04分（上下午等时段字样可出现在日期与时刻之间）。
    let sep = regex::Regex::new(
        r"(?P<y>\d{4})[-_.年](?P<m>\d{1,2})[-_.月](?P<d>\d{1,2})[日\s_-]*(?:(?P<ampm>上午|早上|凌晨|中午|午后|下午|晚上|傍晚)\s*)?(?P<h>\d{1,2})[点時:：._-](?P<min>\d{1,2})(?:(?:分)?(?:[:：._-](?P<s>\d{1,2}))?秒?)?",
    )
    .ok()?;
    // 紧凑格式：20261103_143000 / 20261103143000（纯数字时间戳）
    let compact = regex::Regex::new(
        r"(?P<y>\d{4})(?P<m>\d{2})(?P<d>\d{2})[_-]?(?P<h>\d{2})(?P<min>\d{2})(?:[_-]?(?P<s>\d{2}))?",
    )
    .ok()?;
    let num = |caps: &regex::Captures<'_>, name: &str| {
        caps.name(name).and_then(|m| m.as_str().parse::<u32>().ok())
    };
    for pattern in [&sep, &compact] {
        let Some(caps) = pattern.captures(stem) else {
            continue;
        };
        let (Some(y), Some(mo), Some(d), Some(h), Some(mi)) = (
            num(&caps, "y"),
            num(&caps, "m"),
            num(&caps, "d"),
            num(&caps, "h"),
            num(&caps, "min"),
        ) else {
            continue;
        };
        let s = num(&caps, "s").unwrap_or(0);
        // 下午/晚上等字样 + 12 小时制：小于 12 才加，避免「下午17点」变 29 点。
        let mut h = h;
        if matches!(
            caps.name("ampm").map(|m| m.as_str()),
            Some("下午") | Some("晚上") | Some("午后") | Some("傍晚")
        ) && h < 12
        {
            h += 12;
        }
        let Some(date) = NaiveDate::from_ymd_opt(y as i32, mo, d) else {
            continue;
        };
        let Some(naive) = date.and_hms_opt(h, mi, s) else {
            continue;
        };
        let Some(local) = Local.from_local_datetime(&naive).single() else {
            continue;
        };
        // 录音文件名里的时间不该落在未来（如误匹配到随机数字），明显异常就跳过。
        if local.timestamp() <= chrono::Local::now().timestamp() + 3600 {
            return Some(local.timestamp());
        }
    }
    None
}

/// 文件名是否为常见音频扩展名（课堂录音通常以文件形式发送）。
pub fn is_audio_filename(name: &str) -> bool {
    let ext = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "m4a"
            | "mp3"
            | "wav"
            | "aac"
            | "amr"
            | "ogg"
            | "oga"
            | "flac"
            | "wma"
            | "opus"
            | "3gp"
            | "aiff"
            | "aif"
            | "mp2"
    )
}

/// vault 文件名清洗：路径分隔符与控制字符剔除，长度封顶。
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() > 80 {
        trimmed.chars().take(80).collect()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_extension_matches() {
        for name in ["lec.m4a", "录音.MP3", "x.wav", "class.flac"] {
            assert!(is_audio_filename(name), "{name}");
        }
        for name in ["doc.pdf", "pic.png", "noext", "video.mp4", "note.md"] {
            assert!(!is_audio_filename(name), "{name}");
        }
    }

    #[test]
    fn sanitize_strips_path_chars() {
        assert_eq!(sanitize_name("a/b\\c:d*e?f\"g<h>i|j"), "abcdefghij");
        assert_eq!(sanitize_name("  x  "), "x");
        assert_eq!(sanitize_name(&"长".repeat(100)).chars().count(), 80);
    }

    #[test]
    fn recorded_at_parses_common_recorder_names() {
        use chrono::{Local, TimeZone};
        let ts = |y, mo, d, h, mi: u32, s| {
            Local
                .with_ymd_and_hms(y, mo, d, h, mi, s)
                .single()
                .unwrap()
                .timestamp()
        };
        // 常见录音 app 命名：紧凑、分隔符、中文、带前缀。
        assert_eq!(
            parse_recorded_at("20261103_143000.m4a"),
            Some(ts(2026, 11, 3, 14, 30, 0))
        );
        assert_eq!(
            parse_recorded_at("20261103143000.mp3"),
            Some(ts(2026, 11, 3, 14, 30, 0))
        );
        assert_eq!(
            parse_recorded_at("2026-11-03 14-30-05.m4a"),
            Some(ts(2026, 11, 3, 14, 30, 5))
        );
        assert_eq!(
            parse_recorded_at("Rec_2026.11.03-14.30.m4a"),
            Some(ts(2026, 11, 3, 14, 30, 0))
        );
        assert_eq!(
            parse_recorded_at("录音 2026年11月3日 14_30.mp3"),
            Some(ts(2026, 11, 3, 14, 30, 0))
        );
    }

    #[test]
    fn recorded_at_parses_qq_chinese_time_names() {
        use chrono::{Local, TimeZone};
        let ts = |y, mo, d, h, mi: u32| {
            Local
                .with_ymd_and_hms(y, mo, d, h, mi, 0)
                .single()
                .unwrap()
                .timestamp()
        };
        // QQ 传来的课堂录音常见命名：日期与时刻之间有上午/下午等时段字样。
        assert_eq!(
            parse_recorded_at("2026年11月04日 上午09点11分(1).mp3"),
            Some(ts(2026, 11, 4, 9, 11))
        );
        assert_eq!(
            parse_recorded_at("2026年11月04日 下午05点04分(1).mp3"),
            Some(ts(2026, 11, 4, 17, 4))
        );
        assert_eq!(
            parse_recorded_at("2026年11月04日 下午06点59分(1).mp3"),
            Some(ts(2026, 11, 4, 18, 59))
        );
        assert_eq!(
            parse_recorded_at("录音 2026年11月3日 晚上8点5分.mp3"),
            Some(ts(2026, 11, 3, 20, 5))
        );
    }

    #[test]
    fn recorded_at_rejects_garbage() {
        assert_eq!(parse_recorded_at("数学课录音.m4a"), None);
        assert_eq!(parse_recorded_at("2026-13-40 99-99.m4a"), None);
    }

    #[test]
    fn recorded_time_falls_back_to_received_at() {
        let mut item = PendingItem {
            file: "/tmp/x".into(),
            filename: "数学课录音.m4a".into(),
            size: 1,
            channel: "qq".into(),
            target: "u".into(),
            msg_id: "m1".into(),
            received_at: 1_700_000_000,
            status: "pending".into(),
            error: None,
            course: None,
        };
        assert_eq!(item.recorded_time(), 1_700_000_000);
        item.filename = "20261103_143000.m4a".into();
        assert_ne!(item.recorded_time(), 1_700_000_000);
        assert!(item.recorded_time() > 1_760_000_000);
    }
}
