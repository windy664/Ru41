//! 待转写队列：handler 下载落盘后写入 `lecture.pending` 文档，
//! worker 逐条消费（转写 → Vault 存档 → 入队 agent 整理 → 标记 done）。
//! 状态机：缺省/pending → transcribed（已存档）→ done（已入队 agent）；
//! failed 终态不自动重试（错误会通过 report 暴露，人工排查后删文档重投）。

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
    pub transcript_path: Option<String>,
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
        "m4a" | "mp3"
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
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
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
}
