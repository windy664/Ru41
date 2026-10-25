//! enc 样本库与窗口探测（科研线）。
//!
//! 目标：判定二维码 enc 是否可推导。两条数据腿：
//! - **被动攒样本**：每次照片解码出的 enc（无论签到成败）连同时间戳落 JSONL；
//! - **窗口探测**：QR 活动窗口期间抓学生端详情页 HTML 与 preSign 响应，
//!   侦测平台是否把当前 enc 泄漏给学生可见的接口（若泄漏 = 完全自动化的钥匙）。
//!
//! 样本格式（JSONL，多进程追加写，单行原子性够用）：
//! `{"ts":..,"activity_id":..,"enc":"..","source":"photo|probe","ok":true,"response":".."}`
//! 原始网页工件落 `样本库同目录/probe/{activity_id}/{ts}-{page|presign}.txt`。

use std::io::Write;
use std::path::PathBuf;

/// 一条 enc 样本。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EncSample {
    pub ts: i64,
    pub activity_id: i64,
    pub enc: String,
    /// photo = 照片解码；probe = 窗口页面泄漏。
    pub source: String,
    /// 该 enc 的签到是否被平台接受（probe 样本恒 false——未拿去签）。
    pub ok: bool,
    /// 平台原始响应（拒绝原因/成功文本），拒绝样本是 token 有效期的关键数据。
    pub response: String,
}

/// 追加式样本库；任何 I/O 失败都静默（科研线永不影响签到主链路）。
#[derive(Debug, Clone)]
pub struct EncSampleLogger {
    path: PathBuf,
}

impl EncSampleLogger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn log(&self, sample: &EncSample) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            && let Ok(line) = serde_json::to_string(sample)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// 从 HTML/JSON 文本里抠 enc 候选：只认大写字母数字 16-40 位、
/// 出现在 enc 键附近的串。去重保序。
pub fn extract_enc_candidates(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let patterns = [
        // query 形：...enc=XXXX
        r#"[?&]enc=([A-Z0-9]{16,40})"#,
        // JSON/JS 形："enc":"XXXX" / enc: 'XXXX'
        r#"(?i)["']?enc["']?\s*[:=]\s*["']([A-Z0-9]{16,40})["']"#,
    ];
    for pattern in patterns {
        let Ok(re) = regex::Regex::new(pattern) else {
            continue;
        };
        for cap in re.captures_iter(text) {
            let value = cap[1].to_owned();
            if !out.contains(&value) {
                out.push(value);
            }
        }
    }
    out
}

/// 窗口探测：抓详情页 + preSign，工件落盘，enc 候选进样本库。
/// 全程 best-effort：任何失败只写日志（eprintln），不上报错误。
pub async fn probe_window(
    client: &crate::modules::chaoxing::source::ChaoxingClient,
    activity_id: i64,
    sample_path: &std::path::Path,
) {
    let artifacts = sample_path
        .parent()
        .map(|p| p.join("probe").join(activity_id.to_string()))
        .unwrap_or_else(|| PathBuf::from("/tmp/chaoxing-enc-probe"));
    let _ = std::fs::create_dir_all(&artifacts);
    let logger = EncSampleLogger::new(sample_path);
    let ts = crate::util::now();

    if let Err(err) = client.ensure_session().await {
        eprintln!("[enc-probe] {activity_id} 登录态不可用：{err:#}");
        return;
    }

    for (kind, body) in [
        ("page", client.sign_page_html(activity_id).await),
        (
            "presign",
            client
                .pre_sign_body(
                    &crate::modules::chaoxing::source::ChaoxingCourse::bare(),
                    &activity_id.to_string(),
                    &client.uid().unwrap_or_default(),
                )
                .await,
        ),
    ] {
        match body {
            Ok(text) => {
                let artifact = artifacts.join(format!("{ts}-{kind}.txt"));
                let _ = std::fs::write(&artifact, &text);
                for enc in extract_enc_candidates(&text) {
                    logger.log(&EncSample {
                        ts,
                        activity_id,
                        enc,
                        source: "probe".into(),
                        ok: false,
                        response: format!("leaked-in-{kind}"),
                    });
                }
            }
            Err(err) => {
                eprintln!("[enc-probe] {activity_id} {kind} 抓取失败：{err:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_candidates_from_query_and_json() {
        let html = r#"
            var q = "https://mobilelearn.chaoxing.com/widget/sign/pcStuSignController?activeId=1000175604104&enc=1D0A628CK317F44CCC378M5KD92&c=x";
            {"enc": "ABCDEF123456ABCDEF12", "name": "二维码签到"}
            enc: 'ZYXWVUTSRQPONMLKJ987654'
            小写不算：enc=abcdef123456abcdef123456
        "#;
        let got = extract_enc_candidates(html);
        assert_eq!(
            got,
            vec![
                "1D0A628CK317F44CCC378M5KD92".to_string(),
                "ABCDEF123456ABCDEF12".to_string(),
                "ZYXWVUTSRQPONMLKJ987654".to_string(),
            ]
        );
    }

    #[test]
    fn logger_appends_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("enc-samples.jsonl");
        let logger = EncSampleLogger::new(&path);
        logger.log(&EncSample {
            ts: 1792740630,
            activity_id: 1000175604104,
            enc: "1D0A628CK317F44CCC378M5KD92".into(),
            source: "photo".into(),
            ok: true,
            response: "success".into(),
        });
        logger.log(&EncSample {
            ts: 1792740640,
            activity_id: 1000175604104,
            enc: "ZZZZZZZZZZZZZZZZZZZZ".into(),
            source: "probe".into(),
            ok: false,
            response: "leaked-in-page".into(),
        });
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<EncSample> = Vec::new();
        for line in text.lines() {
            lines.push(serde_json::from_str::<EncSample>(line).unwrap());
        }
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].enc, "1D0A628CK317F44CCC378M5KD92");
        assert!(lines[0].ok);
    }
}
