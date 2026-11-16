//! 待转写队列：handler 下载落盘后写入 `lecture.pending` 文档，
//! worker 逐条消费（切片转写 → 内联 prompt 入队 agent 整理笔记 → 清理工作区）。
//! 状态机：pending → done；failed 终态不自动重试（错误会通过 report 暴露，
//! 人工排查后删文档重投）。vault 只沉淀笔记，转写与音频均为临时产物。

use anyhow::{Context, Result};
use ru41::domain::records::Document;
use ru41::ports::storage::TaskStore;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const PENDING_KIND: &str = "lecture.pending";

/// 待办状态：downloading（后台下载中）→ pending（可转写）→ done / failed。
/// downloading 这段是 2026-11-16 加的：下载甩到后台后，worker 必须跳过它，
/// 否则会去转写还没下完的文件（ffmpeg 直接失败，录音就废了）。
pub const STATUS_DOWNLOADING: &str = "downloading";
/// 待办状态：已下载完，等 worker 转写。
pub const STATUS_PENDING: &str = "pending";

/// 待办扫描窗口（起始档）：日常排队量远小于这个数。
pub const PENDING_SCAN_LIMIT: u32 = 200;
/// 扫描窗口上限：done 的待办记录永远不删（存储接口没有 delete），
/// 历史记录会把窗口占满——窗口必须能放大，否则排在窗口外的录音永远轮不到。
pub const PENDING_SCAN_LIMIT_MAX: u32 = 20_000;

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
    /// downloading | pending | done | failed
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub course: Option<String>,
    /// 转写全文（持久化到记录里）：工作区会被清理，转写不能只活在磁盘上——
    /// 丢了就再也补不回来（2026-11-09 就丢过一次课堂转写）。
    #[serde(default)]
    pub transcript: Option<String>,
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
        recorded_time_of(&self.filename, self.received_at)
    }

    /// 录音时间来源说明（提示词里向 agent 交代依据）。
    pub fn recorded_time_source(&self) -> &'static str {
        if parse_recorded_at(&self.filename).is_some() {
            "来自录音文件名"
        } else if parse_weekday_time(&self.filename).is_some() {
            "来自录音文件名（只有星期，按收到时刻回推到最近一次）"
        } else {
            "文件名无时间，按收到时刻回退"
        }
    }

    /// 能不能交给 worker 转写：不在下载中，也没到终态。
    pub fn is_ready(&self) -> bool {
        !matches!(self.status.as_str(), "done" | "failed" | STATUS_DOWNLOADING)
    }
}

/// 待办里「收到时间最早」的未完成项（先到先转）。
///
/// 不能靠 `documents()` 的返回顺序：它按文档 id 排，而 id 就是 QQ 的 msg_id
/// （一串随机 blob），顺序毫无意义——实测生产库里 id 序与收到时间序完全对不上。
/// 这里把一批待办拉回来自己按 `received_at` 排。
pub fn oldest_unfinished(store: &dyn TaskStore) -> Result<Option<(Document, PendingItem)>> {
    let mut limit = PENDING_SCAN_LIMIT;
    loop {
        let docs = store.documents(PENDING_KIND, limit)?;
        if let Some(picked) = pick_oldest(&docs) {
            return Ok(Some(picked));
        }
        // 窗口取满 = 外面可能还挂着没转写的（历史 done 记录把窗口占满了）→ 放大重扫。
        // 否则排在窗口外的录音会永远轮不到：队列静默饿死，不报错也不转写。
        if docs.len() < limit as usize || limit >= PENDING_SCAN_LIMIT_MAX {
            return Ok(None);
        }
        limit = (limit * 10).min(PENDING_SCAN_LIMIT_MAX);
    }
}

/// 在一批待办文档里挑收到时间最早的一条未完成项。
fn pick_oldest(docs: &[Document]) -> Option<(Document, PendingItem)> {
    docs.iter()
        .filter_map(|doc| {
            let item = PendingItem::from_document(doc).ok()?;
            if !item.is_ready() {
                return None;
            }
            Some((item.received_at, doc.clone(), item))
        })
        .min_by_key(|(received_at, _, _)| *received_at)
        .map(|(_, doc, item)| (doc, item))
}

/// 未完成的待办条数（不含 exclude）：handler 报「前面还有几节」用。
///
/// 必须全量扫才数得准，所以窗口逐级放大到取不满为止。
pub fn unfinished_count(store: &dyn TaskStore, exclude_msg_id: &str) -> Result<usize> {
    let mut limit = PENDING_SCAN_LIMIT;
    let count = loop {
        let docs = store.documents(PENDING_KIND, limit)?;
        let count = docs
            .iter()
            .filter_map(|doc| PendingItem::from_document(doc).ok())
            .filter(|item| {
                item.msg_id != exclude_msg_id && item.status != "done" && item.status != "failed"
            })
            .count();
        if docs.len() < limit as usize || limit >= PENDING_SCAN_LIMIT_MAX {
            break count;
        }
        limit = (limit * 10).min(PENDING_SCAN_LIMIT_MAX);
    };
    Ok(count)
}

/// 上课时间：优先取文件名里解析出的时刻，回退收到时刻。
///
/// handler（即时回执）与 worker（最终归档）共用这一份，
/// 保证回执里说的课程名和最后写进 Vault 的课程一致。
pub fn recorded_time_of(filename: &str, received_at: i64) -> i64 {
    if let Some(ts) = parse_recorded_at(filename) {
        return ts;
    }
    // 文件名只有星期与时刻（如「周四 上午09点01分」）：按收到时刻往前回推最近一次。
    // 录音往往下课后才发出去，用收到时刻会落在课区间外，课表匹配就会失手。
    if let Some((weekday, hour, minute)) = parse_weekday_time(filename)
        && let Some(ts) = resolve_weekday_time(weekday, hour, minute, received_at)
    {
        return ts;
    }
    received_at
}

/// 文件名里只有星期与时刻时的解析（如「周四 上午09点01分」「周三 下午05点04分」）。
/// 返回 (ISO 星期 1..=7, 时, 分)。
pub fn parse_weekday_time(filename: &str) -> Option<(u32, u32, u32)> {
    let weekday_re = regex::Regex::new(r"(?:周|星期|礼拜)\s*([一二三四五六日天1-7])").ok()?;
    let caps = weekday_re.captures(filename)?;
    let raw = caps.get(1)?.as_str();
    let weekday = match raw {
        "一" => 1,
        "二" => 2,
        "三" => 3,
        "四" => 4,
        "五" => 5,
        "六" => 6,
        "日" | "天" => 7,
        other => other.parse::<u32>().ok()?,
    };
    let time_re = regex::Regex::new(
        r"(上午|早上|凌晨|中午|下午|晚上|傍晚)?\s*(\d{1,2})\s*[点時:：]\s*(\d{1,2})",
    )
    .ok()?;
    let caps = time_re.captures(filename)?;
    let mut hour: u32 = caps.get(2)?.as_str().parse().ok()?;
    let minute: u32 = caps.get(3)?.as_str().parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    if matches!(
        caps.get(1).map(|m| m.as_str()),
        Some("下午") | Some("晚上") | Some("午后") | Some("傍晚")
    ) && hour < 12
    {
        hour += 12;
    }
    Some((weekday, hour, minute))
}

/// 把「星期 + 时刻」落到收到时刻之前的最近一次（最多回看一周）。
fn resolve_weekday_time(weekday: u32, hour: u32, minute: u32, anchor: i64) -> Option<i64> {
    use chrono::{Datelike, Local, TimeZone};
    let anchor_local = Local.timestamp_opt(anchor, 0).single()?;
    for back in 0..=7 {
        let date = anchor_local.date_naive() - chrono::Duration::days(back);
        if date.weekday().number_from_monday() != weekday {
            continue;
        }
        let naive = date.and_hms_opt(hour, minute, 0)?;
        let Some(local) = Local.from_local_datetime(&naive).single() else {
            continue;
        };
        // 录音起点不该晚于收到时刻（时钟误差放宽一小时）。
        if local.timestamp() <= anchor + 3600 {
            return Some(local.timestamp());
        }
    }
    None
}

/// 从录音文件名解析开录时刻（本地时区 unix 秒）。覆盖常见录音 app 命名：
/// `20261103_143000`、`20261103143000`、`2026-11-03 14-30-05`、
/// `Rec_2026.11.03-14.30`、`录音 2026年11月3日 14_30`、
/// `2026年11月04日 上午09点11分(1).mp3`（QQ 常见中文时刻格式）、
/// `语音 261116_144234`（两位年份的紧凑命名）。
/// 解析不出返回 None。
pub fn parse_recorded_at(filename: &str) -> Option<i64> {
    use chrono::{Local, NaiveDate, TimeZone};
    // 去扩展名：只剥音频扩展名——无扩展名的名字（`语音 26.11.16 14.42`）若按
    // 「最后一个点」切，会把时刻尾巴切掉。时间串本身可能在任意位置
    // （Rec_ 前缀、「录音 」等字样前后）。
    let stem = match filename.rsplit_once('.') {
        Some((head, ext)) if AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) => head,
        _ => filename,
    };
    // 分隔符格式：2026-11-03 14-30-05 / 2026.11.03-14.30 / 2026年11月3日 14_30
    // / 2026年11月4日 下午05点04分（上下午等时段字样可出现在日期与时刻之间）。
    // 年份 4 位（2026）与 2 位（26）都认。
    let sep = regex::Regex::new(
        r"(?P<y>\d{2,4})[-_.年](?P<m>\d{1,2})[-_.月](?P<d>\d{1,2})[日\s_-]*(?:(?P<ampm>上午|早上|凌晨|中午|午后|下午|晚上|傍晚)\s*)?(?P<h>\d{1,2})[点時:：._-](?P<min>\d{1,2})(?:(?:分)?(?:[:：._-](?P<s>\d{1,2}))?秒?)?",
    )
    .ok()?;
    // 紧凑格式（4 位年份）：20261103_143000 / 20261103143000（纯数字时间戳）
    let compact = regex::Regex::new(
        r"(?P<y>\d{4})(?P<m>\d{2})(?P<d>\d{2})[_-]?(?P<h>\d{2})(?P<min>\d{2})(?:[_-]?(?P<s>\d{2}))?",
    )
    .ok()?;
    // 紧凑格式（2 位年份）：261116_144234 —— 录音 app 常见「语音 261116_144234」命名。
    let compact_short = regex::Regex::new(
        r"(?P<y>\d{2})(?P<m>\d{2})(?P<d>\d{2})[_-]?(?P<h>\d{2})(?P<min>\d{2})(?:[_-]?(?P<s>\d{2}))?",
    )
    .ok()?;
    let num = |caps: &regex::Captures<'_>, name: &str| {
        caps.name(name).and_then(|m| m.as_str().parse::<u32>().ok())
    };
    // 一个名字里可能有多串数字（时刻 + 别的），逐个试到解析出合法时刻为止。
    for pattern in [&sep, &compact, &compact_short] {
        for caps in pattern.captures_iter(stem) {
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
            let Some(date) = NaiveDate::from_ymd_opt(normalize_year(y), mo, d) else {
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
    }
    None
}

/// 文件名里的年份归一：4 位直接用；2 位按 POSIX %y 惯例分界（69-99 → 19xx，00-68 → 20xx）。
fn normalize_year(y: u32) -> i32 {
    match y {
        0..=68 => 2000 + y as i32,
        69..=99 => 1900 + y as i32,
        _ => y as i32,
    }
}

/// 常见音频扩展名（小写，不含点）。
const AUDIO_EXTENSIONS: [&str; 14] = [
    "m4a", "mp3", "wav", "aac", "amr", "ogg", "oga", "flac", "wma", "opus", "3gp", "aiff", "aif",
    "mp2",
];

/// 文件名是否为课堂录音：常见音频扩展名，或「语音/录音 + 时刻」这种无扩展名命名
/// （部分录音 app 导出「语音 261116_144234」，转发后扩展名常常就没了）。
pub fn is_audio_filename(name: &str) -> bool {
    let ext = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        return true;
    }
    Path::new(name).extension().is_none()
        && has_recording_prefix(name)
        && parse_recorded_at(name).is_some()
}

/// 录音类命名前缀：中文「语音/录音/录制」或英文 rec/voice。
fn has_recording_prefix(name: &str) -> bool {
    let name = name.trim_start();
    if name.starts_with("语音") || name.starts_with("录音") || name.starts_with("录制") {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    lower.starts_with("rec") || lower.starts_with("voice")
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
    fn audio_extensionless_recorder_names() {
        // 部分录音 app 导出「语音 261116_144234」这种无扩展名文件：要当录音收下。
        // 用 2025 的日子，保证不在未来（解析器拒绝未来的时刻）。
        assert!(is_audio_filename("语音 250316_144234"));
        assert!(is_audio_filename("录音 250316_144234"));
        assert!(is_audio_filename("voice 250316_144234"));
        // 没有时刻的同前缀文件不算录音，免得把别的文件拖进 ASR。
        assert!(!is_audio_filename("语音备忘"));
        assert!(!is_audio_filename("recording"));
        assert!(!is_audio_filename("语音 250316_144234.txt"));
    }

    #[test]
    fn recorded_at_parses_two_digit_year_names() {
        use chrono::{Local, TimeZone};
        let ts = |y, mo, d, h, mi: u32, s| {
            Local
                .with_ymd_and_hms(y, mo, d, h, mi, s)
                .single()
                .unwrap()
                .timestamp()
        };
        // 「语音 261116_144234」这类两位年份命名（真实例子是 2026-11-16 14:42:34；
        // 这里用 2025 的日子，保证解析器不会因为「未来时刻」把它拒掉）。
        assert_eq!(
            parse_recorded_at("语音 250316_144234.m4a"),
            Some(ts(2025, 3, 16, 14, 42, 34))
        );
        assert_eq!(
            parse_recorded_at("语音 250316_144234"),
            Some(ts(2025, 3, 16, 14, 42, 34))
        );
        // 无扩展名但中间有点：不能把「.42」当扩展名切掉。
        assert_eq!(
            parse_recorded_at("语音 25.03.16 14.42"),
            Some(ts(2025, 3, 16, 14, 42, 0))
        );
        // 两位年份按 POSIX %y 惯例分界：99 → 1999。
        assert_eq!(
            parse_recorded_at("991231_235959.mp3"),
            Some(ts(1999, 12, 31, 23, 59, 59))
        );
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
    fn weekday_time_parses_chinese_names() {
        assert_eq!(parse_weekday_time("周四 上午09点01分.mp3"), Some((4, 9, 1)));
        assert_eq!(
            parse_weekday_time("周三 下午05点04分(1).mp3"),
            Some((3, 17, 4))
        );
        assert_eq!(
            parse_weekday_time("星期日 晚上8点5分.mp3"),
            Some((7, 20, 5))
        );
        assert_eq!(parse_weekday_time("数学课录音.m4a"), None);
        assert_eq!(parse_weekday_time("周四上午.m4a"), None);
    }

    #[test]
    fn weekday_time_resolves_to_nearest_past_occurrence() {
        use chrono::{Local, TimeZone};
        // 2026-11-05 是周四：当天 10:23 收到「周四 上午09点01分」→ 当天 09:01。
        let received = Local
            .with_ymd_and_hms(2026, 11, 5, 10, 23, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(
            resolve_weekday_time(4, 9, 1, received),
            Some(
                Local
                    .with_ymd_and_hms(2026, 11, 5, 9, 1, 0)
                    .single()
                    .unwrap()
                    .timestamp()
            )
        );
        // 2026-10-30 是周五：收到的是「周四」→ 回推到前一天周四。
        let received = Local
            .with_ymd_and_hms(2026, 10, 30, 10, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(
            resolve_weekday_time(4, 9, 1, received),
            Some(
                Local
                    .with_ymd_and_hms(2026, 10, 29, 9, 1, 0)
                    .single()
                    .unwrap()
                    .timestamp()
            )
        );
        // 当天该时刻还没到（08:00 收到「周四 上午09点01分」）：回退到上一个周四。
        let received = Local
            .with_ymd_and_hms(2026, 11, 5, 8, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(
            resolve_weekday_time(4, 9, 1, received),
            Some(
                Local
                    .with_ymd_and_hms(2026, 10, 29, 9, 1, 0)
                    .single()
                    .unwrap()
                    .timestamp()
            )
        );
    }

    #[test]
    fn recorded_time_falls_back_to_received_at() {
        use chrono::{Local, TimeZone};
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
            transcript: None,
        };
        assert_eq!(item.recorded_time(), 1_700_000_000);
        item.filename = "20261103_143000.m4a".into();
        assert_ne!(item.recorded_time(), 1_700_000_000);
        assert!(item.recorded_time() > 1_760_000_000);
        // 只有星期：按收到时刻回推到最近一次。
        let received = Local
            .with_ymd_and_hms(2026, 11, 5, 10, 23, 0)
            .single()
            .unwrap()
            .timestamp();
        item.filename = "周四 上午09点01分.mp3".into();
        item.received_at = received;
        assert_eq!(
            item.recorded_time(),
            Local
                .with_ymd_and_hms(2026, 11, 5, 9, 1, 0)
                .single()
                .unwrap()
                .timestamp()
        );
        assert!(item.recorded_time_source().contains("星期"));
    }
}
