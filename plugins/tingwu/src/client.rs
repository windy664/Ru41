//! 通义听悟（Aliyun Tingwu）客户端：音视频 → 逐字稿。
//!
//! 走的是**网页版内部 API**（不是官方 OpenAPI —— 那套要 AccessKey 签名，
//! 而 transToken 的来源至今没找到）。认证是浏览器 cookie，所以 cookie 放在
//! ru41 的**凭据托管库**里，插件按名字取，真值不进模型上下文。
//!
//! 链路（2026-11-25 在生产机实测跑通，参数逐字段对齐网页版，
//! 网页版的动作序列是从 umi.js 里抠出来的）：
//!   1. `POST /api/trans/request?generatePutLink`（**useSts=0**）→ 拿 OSS 预签名
//!      **putLink**（上传用）+ **getLink**（同步用）+ transId
//!      ⚠️ useSts=1 时服务端**不返回 putLink**（网页版改用 STS 凭证直传 OSS，
//!      那条路要引 OSS SDK，太重）—— 所以这里必须 useSts=0。
//!   2. `PUT putLink`（原始字节，Content-Type 用文件 MIME）→ 文件进 OSS
//!   3. `POST /api/trans/request?syncPutLink`（**fileLink=getLink**！网页版
//!      传的就是 getLink，不是 putLink）→ 服务端开始转写
//!      ⚠️ **没有** startTrans 这个 action —— syncPutLink 成功即自动开转（实测）
//!   4. `POST /api/trans/request?getTransStatus` 轮询 → status 0=成功 1=转写中 2=失败
//!   5. `POST /api/trans/getTransResult` → 逐字稿 JSON 字符串
//!
//! 逐字稿结构：`{pg:[{pi,sc:[{bt,et,id,si,tc}],ui}]}` —— bt/et 毫秒、si 说话人、
//! tc 文本。**空稿**时服务端给的是 `{"sc":[]}`（没有 pg）—— 两种都要认。

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::config::TingwuSection;

const BASE: &str = "https://tingwu.aliyun.com";
/// 浏览器 UA —— 听悟对 UA 有校验。
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// 同一说话人两句话间隔小于这个毫秒数就并进同一段（默认值，可由配置改）。
pub const MERGE_GAP_MS: i64 = 5_000;

/// 转写状态码（听悟 `getTransStatus` 的 `status`）。
pub mod status {
    /// 转写完成。
    pub const SUCCESS: i64 = 0;
    /// 转写中。
    pub const TRANSING: i64 = 1;
    /// 转写失败。
    pub const FAILED: i64 = 2;
    /// 上传成功（还没进转写队列）。
    pub const UPLOADED: i64 = 3;
    /// 等待上传。
    pub const UPLOAD_WAITING: i64 = 4;
    /// 上传中。
    pub const UPLOADING: i64 = 5;
    /// 上传失败。
    pub const UPLOAD_FAILED: i64 = 100;
}

/// 一句话（听悟的最小单位）。
#[derive(Debug, Clone)]
pub struct Sentence {
    pub speaker: u64,
    pub begin_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// 一个说话人段落（连续同一人、间隔短的句子合并）。
#[derive(Debug, Clone)]
pub struct Segment {
    pub speaker: u64,
    pub begin_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// 一个转写任务的结果。
#[derive(Debug, Clone)]
pub struct Transcript {
    pub trans_id: String,
    pub title: String,
    pub duration_secs: f64,
    pub word_count: usize,
    pub sentences: Vec<Sentence>,
    pub segments: Vec<Segment>,
    /// 原始逐字稿（解析后的结构）。
    pub raw: Value,
}

impl Transcript {
    /// 全文（去说话人标签）。
    pub fn full_text(&self) -> String {
        let mut out = String::new();
        for sentence in &self.sentences {
            out.push_str(&sentence.text);
        }
        out
    }

    /// 说话人数量。
    pub fn speaker_count(&self) -> usize {
        self.segments
            .iter()
            .map(|segment| segment.speaker)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
}

/// 历史任务的一条摘要。
#[derive(Debug, Clone)]
pub struct TaskSummary {
    pub trans_id: String,
    pub task_id: String,
    pub show_name: String,
    pub status: i64,
    pub duration_secs: f64,
    pub word_count: usize,
    pub show_time_ms: i64,
}

/// 账号权益（听悟的「剩余时长 / 剩余存储」）。
///
/// ⚠️ **时间单位是秒**（不是毫秒）：实测 `equityAllTimeFlow=4006673`
/// = 1112.96 小时，`dailyEquityList` 里每天签到送 `timeFlow=36000` = 10 小时 ——
/// 按毫秒算就成了「每天送 36 秒」，明显不对。
#[derive(Debug, Clone)]
pub struct Quota {
    /// 累计获得的转写时长（秒）。
    pub total_secs: i64,
    /// 已消耗的转写时长（秒）。
    pub used_secs: i64,
    /// 当前可用时长（秒）—— 取 `maxTimeFlow` 与「总额 - 已用」的较小值。
    pub remaining_secs: i64,
    /// 存储总额（字节）。
    pub storage_total: i64,
    /// 已用存储（字节）。
    pub storage_used: i64,
    /// 账号昵称。
    pub user_name: String,
}

/// 听悟客户端。
pub struct TingwuClient {
    http: reqwest::Client,
    cookie: String,
    poll_timeout: Duration,
    poll_interval: Duration,
    max_file_bytes: u64,
    merge_gap_ms: i64,
    /// 本地 ASR 基址（听悟挂了时的兜底）。
    local_asr_base_url: String,
    /// 本地 ASR 接口形态。
    local_asr_api: String,
}

impl TingwuClient {
    pub fn new(cookie: &str, section: &TingwuSection) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(UA)
            .timeout(Duration::from_secs(120))
            // ⚠️ 必须 no_proxy：容器里注入了出站代理（HTTP_PROXY → mihomo），
            // 而听悟走的是**容器直连**，走代理会拿到空响应
            // （症状是 "EOF while parsing a value"）。同 xfyun.rs 的处理。
            .no_proxy()
            .build()
            .context("构造听悟 HTTP 客户端失败")?;
        Ok(Self {
            http,
            cookie: cookie.to_owned(),
            poll_timeout: section.poll_timeout(),
            poll_interval: section.poll_interval(),
            max_file_bytes: section.max_file_bytes(),
            merge_gap_ms: section.merge_gap_ms(),
            local_asr_base_url: section.local_asr_base_url(),
            local_asr_api: section.local_asr_api(),
        })
    }

    /// 统一 POST：校验听悟的成功标志（`code == "0"` 或 `success == true`）。
    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{BASE}{path}");
        let response = self
            .http
            .post(&url)
            .header("Cookie", &self.cookie)
            .header("Referer", format!("{BASE}/"))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .with_context(|| format!("请求听悟 {path} 失败"))?;
        let text = response
            .text()
            .await
            .with_context(|| format!("读听悟 {path} 的响应失败"))?;
        let parsed: Value = serde_json::from_str(&text).with_context(|| {
            format!(
                "听悟 {path} 返回的不是 JSON: {}",
                &text[..text.len().min(200)]
            )
        })?;
        let code = parsed.get("code").and_then(Value::as_str).unwrap_or("");
        let ok = code == "0"
            || parsed
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || code == "200";
        if !ok {
            let message = parsed
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or(code);
            bail!("听悟 {path} 拒绝了请求: {message}");
        }
        Ok(parsed)
    }

    /// 上传文件并转写，等到出结果。
    pub async fn transcribe(&self, path: &Path) -> Result<Transcript> {
        let meta = tokio::fs::metadata(path)
            .await
            .with_context(|| format!("读文件 {} 失败", path.display()))?;
        if !meta.is_file() {
            bail!("{} 不是普通文件", path.display());
        }
        if meta.len() > self.max_file_bytes {
            bail!(
                "文件太大：{:.1} MB（上限 {:.0} MB）",
                meta.len() as f64 / 1048576.0,
                self.max_file_bytes as f64 / 1048576.0
            );
        }
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("读文件 {} 失败", path.display()))?;
        // 字节数要用两次（上传 + syncPutLink），先记下来。
        let file_size = bytes.len();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "audio".to_owned());
        let (stem, ext) = match name.rsplit_once('.') {
            Some((stem, ext)) => (stem.to_owned(), ext.to_ascii_lowercase()),
            None => (name.clone(), String::new()),
        };
        let task_id = format!("rc-upload-{}-2", chrono::Utc::now().timestamp_millis());
        // 网页版传的是浏览器给的 MIME（umi.js: fileContentType: file.type）。
        let mime = mime_for(&ext);

        // 1) 拿上传地址 + transId（参数逐字段对齐网页版，少一个字段就 DefaultError）
        let tag = json!({
            "showName": stem,
            "fileFormat": ext,
            "fileType": "local",
            "lang": "cn",
            "roleSplitNum": -1,
            "translateSwitch": 0,
            "transTargetValue": 0,
            "originalTag": "{\"isVideo\":0}",
            "client": "web",
        });
        let body = json!({
            "action": "generatePutLink",
            "version": "1.0",
            "taskId": task_id,
            // ⚠️ 必须 0：useSts=1 时服务端只给 sts 不给 putLink（没法上传）。
            "useSts": 0,
            "fileSize": file_size,
            "dirId": 0,
            "fileContentType": mime,
            "tag": tag,
        });
        let data = self
            .post("/api/trans/request?generatePutLink&c=web", &body)
            .await?
            .get("data")
            .cloned()
            .context("generatePutLink 没返回 data")?;
        let put_link = data
            .get("putLink")
            .and_then(Value::as_str)
            .context("generatePutLink 没返回 putLink（useSts 必须是 0）")?
            .to_owned();
        let get_link = data
            .get("getLink")
            .and_then(Value::as_str)
            .context("generatePutLink 没返回 getLink")?
            .to_owned();
        let trans_id = data
            .get("transId")
            .and_then(Value::as_str)
            .context("generatePutLink 没返回 transId")?
            .to_owned();

        // 2) PUT 上传到 OSS（预签名地址，直接传原始字节）
        let response = self
            .http
            .put(&put_link)
            .header("Content-Type", mime)
            .body(bytes)
            .send()
            .await
            .context("上传到 OSS 失败")?;
        if !response.status().is_success() {
            bail!("上传到 OSS 失败: HTTP {}", response.status());
        }

        // 3) 通知服务端：文件已上传 → 服务端自动开转（没有 startTrans 这个 action）
        //    ⚠️ fileLink 传 **getLink**（网页版就是这么传的，umi.js 的 syncPutLink
        //    拿的是 d.getLink）；传 putLink 任务会卡在 upload-waiting。
        let sync = json!({
            "action": "syncPutLink",
            "version": "1.0",
            "fileLink": get_link,
            "fileSize": file_size,
            "transId": trans_id,
        });
        self.post("/api/trans/request?syncPutLink&c=web", &sync)
            .await?;

        // 4) 轮询状态
        let transcript = self.wait_for_result(&trans_id, &stem).await?;
        Ok(transcript)
    }

    /// 轮询直到出结果，再取逐字稿。
    async fn wait_for_result(&self, trans_id: &str, title: &str) -> Result<Transcript> {
        let poll = json!({
            "action": "getTransStatus",
            "version": "1.0",
            "userId": "",
            "transIds": [trans_id],
            "taskIds": [trans_id],
            "preview": 1,
        });
        let deadline = Instant::now() + self.poll_timeout;
        let mut last: Option<(i64, Option<String>)> = None;
        while Instant::now() < deadline {
            let result = self
                .post("/api/trans/request?getTransStatus&c=web", &poll)
                .await?;
            let row = result
                .get("data")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .cloned();
            if let Some(row) = row {
                let state = row.get("status").and_then(Value::as_i64).unwrap_or(-1);
                let message = row
                    .get("statusMsg")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                last = Some((state, message.clone()));
                match state {
                    status::SUCCESS => break,
                    status::FAILED | status::UPLOAD_FAILED => {
                        bail!(
                            "听悟转写失败（status={state}）：{}",
                            message.unwrap_or_else(|| "没给原因".to_owned())
                        );
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(self.poll_interval).await;
        }
        match last {
            Some((status::SUCCESS, _)) => {}
            Some((state, message)) => {
                bail!(
                    "听悟转写没成功（status={state}）：{}",
                    message.unwrap_or_else(|| "没给原因".to_owned())
                );
            }
            None => bail!("听悟转写超时（等不到状态）"),
        }

        // 5) 取逐字稿
        self.result(trans_id, title).await
    }

    /// 取某个已有任务的逐字稿。
    pub async fn result(&self, trans_id: &str, title: &str) -> Result<Transcript> {
        let body = json!({
            "action": "getTransResult",
            "version": "1.0",
            "transId": trans_id,
        });
        let result = self.post("/api/trans/getTransResult?c=web", &body).await?;
        let data = result.get("data").cloned().unwrap_or(Value::Null);
        let raw = data
            .get("result")
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .unwrap_or(Value::Null);
        if raw.is_null() {
            bail!("听悟任务 {trans_id} 还没有逐字稿（可能还在转写）");
        }
        let sentences = parse_sentences(&raw);
        let segments = group_by_speaker(&sentences, self.merge_gap_ms);
        Ok(Transcript {
            trans_id: trans_id.to_owned(),
            title: title.to_owned(),
            duration_secs: data
                .get("duration")
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            word_count: data
                .get("wordCount")
                .and_then(Value::as_u64)
                .unwrap_or_default() as usize,
            sentences,
            segments,
            raw,
        })
    }

    /// 列出历史转写任务。
    pub async fn list(&self, page_size: u64) -> Result<Vec<TaskSummary>> {
        let body = json!({
            "action": "getTransList",
            "version": "1.0",
            "userId": "",
            "filter": {
                "status": [], "fileTypes": [], "beginTime": "", "endTime": "",
                "showName": "", "read": "", "lang": "", "shareUserId": "", "client": ""
            },
            "preview": 1,
            "pageNo": 1,
            "pageSize": page_size,
        });
        let result = self
            .post("/api/trans/request?getTransList&c=web", &body)
            .await?;
        let rows = result
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(TaskSummary {
                    trans_id: row.get("transId")?.as_str()?.to_owned(),
                    task_id: row.get("taskId")?.as_str()?.to_owned(),
                    show_name: row
                        .get("tag")
                        .and_then(|tag| tag.get("showName"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    status: row.get("status").and_then(Value::as_i64).unwrap_or(-1),
                    duration_secs: row.get("duration").and_then(Value::as_f64).unwrap_or(0.0),
                    word_count: row.get("wordCount").and_then(Value::as_u64).unwrap_or(0) as usize,
                    show_time_ms: row.get("showTime").and_then(Value::as_i64).unwrap_or(0),
                })
            })
            .collect())
    }

    /// 查账号权益：剩余转写时长 / 剩余存储。
    ///
    /// 接口是 `GET|POST /api/subscription/equity/queryUserEquityInfo`（不是
    /// `/api/trans/request` 那一套，所以单独走，不复用 [`Self::post`] 的
    /// `?action=` 约定）。字段：`equityAllTimeFlow`（总额，秒）、
    /// `equityExpendTimeFlow`（已用，秒）、`maxTimeFlow`（当前上限，秒）、
    /// `equityAllStorage` / `equityExpendStorage`（字节）。
    pub async fn quota(&self) -> Result<Quota> {
        let url = format!("{BASE}/api/subscription/equity/queryUserEquityInfo");
        let response = self
            .http
            .post(&url)
            .header("Cookie", &self.cookie)
            .header("Referer", format!("{BASE}/equity"))
            .header("Content-Type", "application/json")
            .json(&json!({}))
            .send()
            .await
            .context("请求听悟权益接口失败")?;
        let text = response.text().await.context("读听悟权益接口的响应失败")?;
        let parsed: Value = serde_json::from_str(&text).with_context(|| {
            format!(
                "听悟权益接口返回的不是 JSON: {}",
                &text[..text.len().min(200)]
            )
        })?;
        let data = parsed
            .get("data")
            .cloned()
            .context("听悟权益接口没返回 data")?;
        let total = data
            .get("equityAllTimeFlow")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let used = data
            .get("equityExpendTimeFlow")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let cap = data.get("maxTimeFlow").and_then(Value::as_i64).unwrap_or(0);
        // 剩余 = min(上限, 总额-已用)：两个口径取小的那个，保守。
        let by_diff = (total - used).max(0);
        let remaining = if cap > 0 { cap.min(by_diff) } else { by_diff };
        Ok(Quota {
            total_secs: total,
            used_secs: used,
            remaining_secs: remaining,
            storage_total: data
                .get("equityAllStorage")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            storage_used: data
                .get("equityExpendStorage")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            user_name: data
                .get("aliyunUserName")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        })
    }

    /// 删除云端转写任务。
    ///
    /// `permanent = true` → 直接永久删除（不进回收站，不可恢复）；
    /// `permanent = false` → 进回收站（之后还能 `revertTrashes` 捞回来）。
    pub async fn delete_trans(&self, trans_ids: &[String], permanent: bool) -> Result<()> {
        if trans_ids.is_empty() {
            bail!("没给要删的任务 ID");
        }
        let body = json!({
            "action": "delTrans",
            "version": "1.0",
            "userId": "",
            "transIds": trans_ids,
            "deletePermanently": permanent,
        });
        self.post("/api/trans/request?delTrans&c=web", &body)
            .await?;
        Ok(())
    }

    /// 清空回收站（听悟的「垃圾」就堆在这儿 —— delTrans 不带永久标记时先进回收站）。
    pub async fn empty_trash(&self) -> Result<()> {
        let body = json!({
            "action": "delAllTrashes",
            "version": "1.0",
        });
        self.post("/api/trash/request?delAllTrashes&c=web", &body)
            .await?;
        Ok(())
    }

    /// 本地 ASR 兜底转写（听悟配额用完 / 网络挂时用）。
    ///
    /// ⚠️ 本地这条**没有说话人分离**（whisper.cpp / paraformer 都不给 `si`），
    /// 所以结果是一整段文字、segments 为空 —— 有字就行。
    pub async fn transcribe_local(&self, path: &Path) -> Result<Transcript> {
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("读文件 {} 失败", path.display()))?;
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "audio.mp3".to_owned());
        let endpoint = if self.local_asr_api == "openai" {
            format!("{}/v1/audio/transcriptions", self.local_asr_base_url)
        } else {
            format!("{}/inference", self.local_asr_base_url)
        };
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str("audio/mpeg")
            .context("构造本地 ASR 的上传块失败")?;
        let mut form = reqwest::multipart::Form::new().part("file", part);
        if self.local_asr_api == "openai" {
            form = form.text("model", "whisper-1").text("stream", "false");
        } else {
            form = form.text("response-format", "json");
        }
        let response = self
            .http
            .post(&endpoint)
            .multipart(form)
            .send()
            .await
            .with_context(|| format!("本地 ASR（{endpoint}）请求失败"))?;
        let status = response.status();
        let text = response.text().await.context("读本地 ASR 的响应失败")?;
        if !status.is_success() {
            bail!(
                "本地 ASR 返回 HTTP {status}: {}",
                &text[..text.len().min(200)]
            );
        }
        let parsed: Value = serde_json::from_str(&text).with_context(|| {
            format!("本地 ASR 返回的不是 JSON: {}", &text[..text.len().min(200)])
        })?;
        let content = parsed
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| parsed.get("content").and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("本地 ASR 没给转写文本")?
            .to_owned();
        let title = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "audio".to_owned());
        Ok(Transcript {
            trans_id: format!("local-{title}"),
            title,
            duration_secs: 0.0,
            word_count: content.chars().filter(|c| !c.is_whitespace()).count(),
            sentences: vec![Sentence {
                speaker: 0,
                begin_ms: 0,
                end_ms: 0,
                text: content,
            }],
            segments: Vec::new(),
            raw: parsed,
        })
    }
}

/// 扩展名 → MIME（网页版传的是浏览器的 file.type；服务端认这个）。
pub fn mime_for(ext: &str) -> &'static str {
    match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "amr" => "audio/amr",
        "wma" => "audio/x-ms-wma",
        "mp4" => "video/mp4",
        "m4v" => "video/x-m4v",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "wmv" => "video/x-ms-wmv",
        "mpeg" | "mpg" => "video/mpeg",
        _ => "application/octet-stream",
    }
}

/// 解析逐字稿成句子列表。
///
/// 两种形态都要认（实测）：
/// - 正常：`{pg:[{pi,sc:[…],ui}]}` —— 按页分组
/// - 空稿：`{sc:[]}` —— 没有 pg，句子直接挂在顶层
pub fn parse_sentences(raw: &Value) -> Vec<Sentence> {
    let mut out = Vec::new();
    fn collect(sentences: &Value, out: &mut Vec<Sentence>) {
        let Some(items) = sentences.as_array() else {
            return;
        };
        for sentence in items {
            let text = sentence.get("tc").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                continue;
            }
            out.push(Sentence {
                speaker: sentence
                    .get("si")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .max(0) as u64,
                begin_ms: sentence.get("bt").and_then(Value::as_i64).unwrap_or(0),
                end_ms: sentence.get("et").and_then(Value::as_i64).unwrap_or(0),
                text: text.to_owned(),
            });
        }
    }
    if let Some(pages) = raw.get("pg").and_then(Value::as_array) {
        for page in pages {
            if let Some(sentences) = page.get("sc") {
                collect(sentences, &mut out);
            }
        }
    } else if let Some(sentences) = raw.get("sc") {
        collect(sentences, &mut out);
    }
    out
}

/// 按说话人合并句子：同一人且与上一句间隔 < `gap_ms` 的并进同一段。
pub fn group_by_speaker(sentences: &[Sentence], gap_ms: i64) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    for sentence in sentences {
        match out.last_mut() {
            Some(last)
                if last.speaker == sentence.speaker && sentence.begin_ms - last.end_ms < gap_ms =>
            {
                last.text.push_str(&sentence.text);
                last.end_ms = sentence.end_ms;
            }
            _ => out.push(Segment {
                speaker: sentence.speaker,
                begin_ms: sentence.begin_ms,
                end_ms: sentence.end_ms,
                text: sentence.text.clone(),
            }),
        }
    }
    out
}

/// 绝对时间戳（毫秒）→ 本地日期时间 `MM-DD HH:MM`。
///
/// ⚠️ 别拿 [`format_ts`] 渲染它 —— 那是**时长**（从 0 数起）；
/// showTime 是绝对 epoch 毫秒，用时长格式会渲染成「498781:51:41」这种鬼话。
pub fn format_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|stamp| {
            stamp
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// 毫秒 → `HH:MM:SS`（超过一小时才带小时）。
pub fn format_ts(ms: i64) -> String {
    let total = (ms.max(0) / 1000) as u64;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// 渲染成 Markdown（按说话人分段，带时间戳）。
pub fn render_markdown(transcript: &Transcript) -> String {
    let mut out = format!(
        "# {}\n\n- 时长 {} · {} 字 · {} 段 · {} 个说话人 · 任务 {}\n",
        transcript.title,
        format_ts((transcript.duration_secs * 1000.0) as i64),
        transcript.word_count,
        transcript.segments.len(),
        transcript.speaker_count(),
        transcript.trans_id
    );
    for segment in &transcript.segments {
        out.push_str(&format!(
            "\n**说话人 {}**（{} - {}）\n\n{}\n",
            segment.speaker,
            format_ts(segment.begin_ms),
            format_ts(segment.end_ms),
            segment.text
        ));
    }
    out
}

/// 渲染成纯文本（每行一句，带时间戳与说话人）。
pub fn render_text(transcript: &Transcript) -> String {
    let mut out = String::new();
    for sentence in &transcript.sentences {
        out.push_str(&format!(
            "[{}] 说话人{}：{}\n",
            format_ts(sentence.begin_ms),
            sentence.speaker,
            sentence.text
        ));
    }
    out
}

/// 渲染成 SRT 字幕（按句子切）。
pub fn render_srt(transcript: &Transcript) -> String {
    let mut out = String::new();
    for (index, sentence) in transcript.sentences.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            index + 1,
            srt_ts(sentence.begin_ms),
            srt_ts(sentence.end_ms),
            sentence.text
        ));
    }
    out
}

/// 毫秒 → SRT 时间戳 `HH:MM:SS,mmm`。
fn srt_ts(ms: i64) -> String {
    let ms = ms.max(0);
    let total = (ms / 1000) as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        total / 3600,
        (total % 3600) / 60,
        total % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Sentence> {
        vec![
            Sentence {
                speaker: 0,
                begin_ms: 30,
                end_ms: 506,
                text: "我们".into(),
            },
            Sentence {
                speaker: 0,
                begin_ms: 600,
                end_ms: 1200,
                text: "自己".into(),
            },
            Sentence {
                speaker: 1,
                begin_ms: 1300,
                end_ms: 2000,
                text: "好".into(),
            },
        ]
    }

    #[test]
    fn same_speaker_merges_and_different_speaker_splits() {
        let segments = group_by_speaker(&sample(), 5_000);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].text, "我们自己");
        assert_eq!(segments[0].begin_ms, 30);
        assert_eq!(segments[0].end_ms, 1200);
        assert_eq!(segments[1].speaker, 1);
    }

    #[test]
    fn big_gap_splits_same_speaker() {
        let mut sentences = sample();
        sentences[1].begin_ms = 60_000;
        sentences[1].end_ms = 61_000;
        let segments = group_by_speaker(&sentences, 5_000);
        assert_eq!(segments.len(), 3);
    }

    #[test]
    fn parses_tingwu_shape() {
        let raw = json!({
            "pg": [{
                "sc": [
                    {"bt": 30, "et": 506, "id": 10, "si": 1, "tc": "我们"},
                    {"bt": 600, "et": 900, "id": 11, "si": 2, "tc": "好"}
                ]
            }]
        });
        let sentences = parse_sentences(&raw);
        assert_eq!(sentences.len(), 2);
        assert_eq!(sentences[0].speaker, 1);
        assert_eq!(sentences[1].text, "好");
    }

    #[test]
    fn parses_empty_transcript_shape() {
        // 空稿服务端给的是 {"sc":[]}（没有 pg）—— 别当成解析失败。
        let raw = json!({"sc": []});
        assert!(parse_sentences(&raw).is_empty());
    }

    #[test]
    fn mime_matches_browser_types() {
        assert_eq!(mime_for("mp3"), "audio/mpeg");
        assert_eq!(mime_for(".M4A"), "audio/mp4");
        assert_eq!(mime_for("mp4"), "video/mp4");
        assert_eq!(mime_for("xyz"), "application/octet-stream");
    }

    #[test]
    fn timestamp_formatting() {
        assert_eq!(format_ts(0), "00:00");
        assert_eq!(format_ts(65_000), "01:05");
        assert_eq!(format_ts(3_725_000), "01:02:05");
    }

    #[test]
    fn absolute_time_is_not_rendered_as_duration() {
        // showTime 是绝对 epoch 毫秒 —— 用时长格式会渲染成「498781:51:41」。
        let text = format_time(1_795_614_000_000);
        assert!(!text.contains(":51:41"), "绝对时间不该按时长渲染：{text}");
        assert!(text.len() >= 10, "至少是 YYYY-MM-DD 的长度：{text}");
    }

    #[test]
    fn srt_timestamp_has_milliseconds() {
        assert_eq!(srt_ts(3_725_456), "01:02:05,456");
    }

    #[test]
    fn markdown_shows_speaker_one_as_one() {
        // si 是 1 起的 —— 渲染不能再 +1（不然「说话人 1」永远不存在）。
        let transcript = Transcript {
            trans_id: "t1".into(),
            title: "x".into(),
            duration_secs: 1.0,
            word_count: 1,
            sentences: vec![Sentence {
                speaker: 1,
                begin_ms: 0,
                end_ms: 500,
                text: "喂".into(),
            }],
            segments: vec![Segment {
                speaker: 1,
                begin_ms: 0,
                end_ms: 500,
                text: "喂".into(),
            }],
            raw: Value::Null,
        };
        let markdown = render_markdown(&transcript);
        assert!(markdown.contains("说话人 1"), "{markdown}");
        assert!(!markdown.contains("说话人 2"), "{markdown}");
    }
}
