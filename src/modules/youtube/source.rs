//! yt-dlp 子进程客户端：频道视频列表与英文字幕转写。
//!
//! YouTube RSS 端点（feeds/videos.xml）对本部署 404（代理与直连均验证），
//! 采集统一走 yt-dlp：`--flat-playlist --print` 拿列表，`--write(-auto)-sub` 拿字幕。
//! 网络失败只影响本轮，由上层 service 决定重试策略。

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;
use tokio::process::Command;

/// 列表条目（flat playlist，无网络外的额外请求）。
#[derive(Debug, Clone, Deserialize)]
pub struct VideoEntry {
    pub id: String,
    pub title: String,
    /// 秒；flat 模式缺失时为 None（不过滤）。
    pub duration_secs: Option<u64>,
    pub view_count: Option<u64>,
    /// 发布时间（Unix 秒）；flat 模式通常缺失，缺失时由 service 补元数据判断。
    pub timestamp: Option<u64>,
}

/// 单视频元数据（video_meta 返回）。
#[derive(Debug, Clone)]
pub struct VideoMeta {
    pub title: String,
    pub duration_secs: Option<u64>,
    /// 发布时间（Unix 秒），用于轮询路径的新鲜度兜底。
    pub timestamp: Option<u64>,
}

/// 字幕转写段：起点秒 + 该句文本（标签已剥离、滚动重复已合并）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptSegment {
    pub start_secs: u32,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct YtDlpClient {
    bin: String,
    proxy_url: Option<String>,
    ytdlp_cookies: Option<String>,
}

impl YtDlpClient {
    pub fn new(
        bin: impl Into<String>,
        proxy_url: Option<String>,
        ytdlp_cookies: Option<String>,
    ) -> Self {
        Self {
            bin: bin.into(),
            proxy_url,
            ytdlp_cookies,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        command.kill_on_drop(true);
        // 字幕接口对数据中心/直连 IP 风控（429/bot 校验），带账号 cookies 可过。
        if let Some(cookies) = &self.ytdlp_cookies {
            command.arg("--cookies").arg(cookies);
        }
        if let Some(proxy) = &self.proxy_url {
            command.env("HTTP_PROXY", proxy).env("HTTPS_PROXY", proxy);
            command.env("http_proxy", proxy).env("https_proxy", proxy);
        }
        command
    }

    /// 频道最新视频列表（newest first）。失败返回错误，由调用方记入本轮 errors。
    pub async fn list_videos(
        &self,
        channel_videos_url: &str,
        limit: u32,
    ) -> Result<Vec<VideoEntry>> {
        // \u{1f} 作字段分隔，避免标题内含制表符时串列。
        let template =
            "%(id)s\u{1f}%(title)s\u{1f}%(duration)s\u{1f}%(view_count)s\u{1f}%(timestamp)s";
        // playlist-items 用区间语法：裸数字是「只取第 N 条」而非「前 N 条」。
        let items = format!("1:{limit}");
        let output = self
            .command()
            .args([
                "--flat-playlist",
                "--no-warnings",
                "--playlist-items",
                &items,
                "--print",
                template,
                channel_videos_url,
            ])
            .output()
            .await
            .with_context(|| format!("spawn {} list", self.bin))?;
        if !output.status.success() {
            bail!("yt-dlp list failed: {}", tail_stderr(&output.stderr));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut entries = Vec::new();
        for line in stdout.lines() {
            let fields: Vec<&str> = line.split('\u{1f}').collect();
            if fields.len() < 2 {
                continue;
            }
            let id = fields[0].trim();
            if id.is_empty() || id == "NA" {
                continue;
            }
            let title = fields[1].trim();
            entries.push(VideoEntry {
                id: id.to_string(),
                title: if title == "NA" {
                    id.to_string()
                } else {
                    title.to_string()
                },
                duration_secs: parse_count(fields.get(2).copied()),
                view_count: parse_count(fields.get(3).copied()),
                timestamp: parse_count(fields.get(4).copied()),
            });
        }
        Ok(entries)
    }

    /// 单视频元数据（标题/时长/发布时间），手动点播与轮询新鲜度兜底共用。
    pub async fn video_meta(&self, video_id: &str) -> Result<VideoMeta> {
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        let output = self
            .command()
            .args([
                "--skip-download",
                "--no-warnings",
                "--no-playlist",
                "--print",
                "%(title)s\u{1f}%(duration)s\u{1f}%(timestamp)s",
                &url,
            ])
            .output()
            .await
            .with_context(|| format!("spawn {} meta", self.bin))?;
        if !output.status.success() {
            bail!("yt-dlp meta failed: {}", tail_stderr(&output.stderr));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().next().unwrap_or("");
        let fields: Vec<&str> = line.split('\u{1f}').collect();
        let title = fields.first().copied().unwrap_or("").trim();
        let title = if title.is_empty() || title == "NA" {
            video_id.to_string()
        } else {
            title.to_string()
        };
        Ok(VideoMeta {
            title,
            duration_secs: parse_count(fields.get(1).copied()),
            timestamp: parse_count(fields.get(2).copied()),
        })
    }

    /// 拉取英文字幕并解析为转写段；优先创作者手动字幕，缺省退回 ASR 自动字幕。
    pub async fn transcript(&self, video_id: &str) -> Result<Vec<TranscriptSegment>> {
        let dir = tempfile::tempdir().context("create subtitle tempdir")?;
        let stem = dir.path().join("sub");
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        for auto in [false, true] {
            let mut command = self.command();
            command.args([
                "--skip-download",
                "--no-warnings",
                "--no-playlist",
                "--sub-langs",
                "en.*",
                "--sub-format",
                "vtt",
                "-o",
            ]);
            command.arg(stem.to_string_lossy().to_string());
            if auto {
                command.arg("--write-auto-sub");
            } else {
                command.arg("--write-sub");
            }
            command.arg(&url);
            let output =
                tokio::time::timeout(std::time::Duration::from_secs(180), command.output())
                    .await
                    .with_context(|| format!("spawn {} subtitles", self.bin))?
                    .with_context(|| format!("run {} subtitles", self.bin))?;
            if !output.status.success() {
                bail!("yt-dlp subtitles failed: {}", tail_stderr(&output.stderr));
            }
            if let Some(path) = find_subtitle(&stem, dir.path())? {
                let raw = tokio::fs::read_to_string(&path)
                    .await
                    .with_context(|| format!("read {}", path.display()))?;
                let segments = parse_vtt(&raw);
                if !segments.is_empty() {
                    return Ok(segments);
                }
            }
        }
        bail!("no English subtitles available for {video_id}")
    }
}

/// yt-dlp 语言后缀不固定（.en.vtt / .en-orig.vtt …），按前缀匹配找产物。
fn find_subtitle(stem: &Path, dir: &Path) -> Result<Option<std::path::PathBuf>> {
    let stem = stem.to_string_lossy().to_string();
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("scan {}", dir.display()))? {
        let path = entry
            .with_context(|| format!("scan {}", dir.display()))?
            .path();
        let name = path.to_string_lossy().to_string();
        if name.starts_with(&stem) && name.ends_with(".vtt") {
            matches.push(path);
        }
    }
    matches.sort();
    Ok(matches.into_iter().next())
}

fn parse_count(field: Option<&str>) -> Option<u64> {
    field?.trim().split('.').next()?.parse().ok()
}

fn tail_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.len() <= 300 {
        text.to_string()
    } else {
        text[text.len() - 300..].to_string()
    }
}

/// 解析 VTT：剥标签、去连续重复、合并滚动重叠（YouTube ASR 的逐词上卷格式）。
pub fn parse_vtt(raw: &str) -> Vec<TranscriptSegment> {
    let mut segments: Vec<TranscriptSegment> = Vec::new();
    for block in raw.split("\n\n") {
        let mut start_secs: Option<u32> = None;
        for line in block.lines() {
            if let Some(secs) = cue_start_secs(line) {
                start_secs = Some(secs);
                continue;
            }
            let Some(secs) = start_secs else { continue };
            let text = strip_tags(line);
            if text.is_empty() {
                continue;
            }
            if segments.last().map(|s| s.text.as_str()) == Some(text.as_str()) {
                continue;
            }
            segments.push(TranscriptSegment {
                start_secs: secs,
                text,
            });
        }
    }
    merge_rolling(segments)
}

/// 时间轴行 "00:00:04.240 --> 00:00:06.230 ..." → 起点秒。
fn cue_start_secs(line: &str) -> Option<u32> {
    let timing = line.split("-->").next()?.trim();
    let (clock, _) = timing.split_once('.')?;
    let nums: Vec<u32> = clock
        .split(':')
        .map(|part| part.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    Some(match nums.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => return None,
    })
}

fn strip_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut depth = 0usize;
    for ch in line.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// 上一段结尾与下一段开头重叠 ≥3 字节时拼接（按 char boundary 对齐）。
fn merge_rolling(mut segments: Vec<TranscriptSegment>) -> Vec<TranscriptSegment> {
    let mut merged: Vec<TranscriptSegment> = Vec::with_capacity(segments.len());
    for segment in segments.drain(..) {
        if let Some(last) = merged.last_mut() {
            let max = last.text.len().min(segment.text.len());
            let mut overlap = 0usize;
            for k in (3..=max).rev() {
                if last.text.is_char_boundary(last.text.len() - k)
                    && segment.text.is_char_boundary(k)
                    && last.text.ends_with(&segment.text[..k])
                {
                    overlap = k;
                    break;
                }
            }
            if overlap > 0 {
                last.text.push_str(&segment.text[overlap..]);
                continue;
            }
        }
        merged.push(segment);
    }
    merged
}

/// 渲染为带行号与时间戳的转写文本；超长截断（长视频/播客保护 prompt 额度）。
/// 至少保留第一行，保证截断后的预习单仍有可用上下文。
pub fn render_transcript(segments: &[TranscriptSegment], max_chars: usize) -> String {
    let mut out = String::new();
    for (index, segment) in segments.iter().enumerate() {
        let line = format!(
            "[L{} {}] {}\n",
            index + 1,
            format_ts(u64::from(segment.start_secs)),
            segment.text
        );
        if !out.is_empty() && out.len() + line.len() > max_chars {
            out.push_str("...（转写过长已截断）\n");
            break;
        }
        out.push_str(&line);
    }
    out
}

pub fn format_ts(secs: u64) -> String {
    let (h, rem) = (secs / 3600, secs % 3600);
    if h > 0 {
        format!("{h}:{:02}:{:02}", rem / 60, rem % 60)
    } else {
        format!("{}:{:02}", rem / 60, rem % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_VTT: &str = r#"WEBVTT
Kind: captions
Language: en

00:00:04.240 --> 00:00:06.230 align:start position:0%
forever, twice. And Tesla finally
unveiled<00:00:04.600><c> the</c><00:00:04.680><c> Roadster</c>

00:00:06.230 --> 00:00:06.240 align:start position:0%
unveiled the Roadster


00:01:02.000 --> 00:01:04.000 align:start position:0%
trained from scratch by hand
"#;

    #[test]
    fn parses_vtt_with_rolling_dedup() {
        let segments = parse_vtt(SAMPLE_VTT);
        // cue1 的第二行（标签已剥）与 cue2 的整行完全相同，只保留一份。
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].start_secs, 4);
        assert_eq!(segments[0].text, "forever, twice. And Tesla finally");
        assert_eq!(segments[1].text, "unveiled the Roadster");
        assert_eq!(segments[2].start_secs, 62);
        assert_eq!(segments[2].text, "trained from scratch by hand");
    }

    #[test]
    fn merges_partial_overlap_across_cues() {
        let raw = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\ntrained from\n\n\
                   00:00:02.000 --> 00:00:03.000\nfrom scratch\n";
        let segments = parse_vtt(raw);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "trained from scratch");
    }

    #[test]
    fn rejects_empty_vtt() {
        assert!(parse_vtt("WEBVTT\n").is_empty());
    }

    #[test]
    fn renders_numbered_lines_with_truncation() {
        let segments = vec![
            TranscriptSegment {
                start_secs: 4,
                text: "hello world".into(),
            },
            TranscriptSegment {
                start_secs: 65,
                text: "second line".into(),
            },
        ];
        let full = render_transcript(&segments, 10_000);
        assert_eq!(full, "[L1 0:04] hello world\n[L2 1:05] second line\n");

        let truncated = render_transcript(&segments, 20);
        assert!(truncated.starts_with("[L1 0:04] hello world\n"));
        assert!(truncated.contains("截断"));
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_ts(4), "0:04");
        assert_eq!(format_ts(65), "1:05");
        assert_eq!(format_ts(3_723), "1:02:03");
    }
}
