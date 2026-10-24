//! 教务系统（正方 jwglxt）协议客户端：联奕 CAS 单点登录 + 课表查询。
//!
//! 登录链路（协议调研见 docs/jwxt-recon.md）：
//! 1. CAS REST `POST /v1/tickets`（Ohdave 裸 RSA 密码；该校强制图形验证码，
//!    为算术题，模块内置模板匹配求解器自动完成，见 captcha.rs）；
//! 2. `POST /v1/tickets/<TGT>` 换 ST（免验证码）；
//! 3. `GET <jwxt>/sso/lyiotlogin?ticket=<ST>` 302 链建立教务会话；
//! 4. `POST /jwglxt/kbcx/xskbcx_cxXsgrkb.html?gnmkdm=N2151` 拉整学期课表。
//!
//! 会话（TGT + cookie）持久化到本地文件；实测该校 TGT 时效很短（分钟级），
//! 所以轮询路径 = 会话复用 → TGT 换 ST → 自动验证码登录，全程无需人工。
//! 仅当密码错误/账号锁定等硬失败时才提醒人工介入。

use anyhow::{Context, Result, bail};
use base64::Engine;
use num_bigint::BigUint;
use num_traits::{Num, Zero};
use reqwest::cookie::CookieStore;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 从基址 URL 提取主机名（DoH 兜底按主机注入 resolve 表）。
fn host_of(base: &str) -> String {
    url::Url::parse(base)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_owned()))
        .unwrap_or_default()
}

/// 统一构建教务 HTTP 客户端：UA/超时/cookie jar + DNS 兜底覆盖
/// （系统解析失败的 host 走公共 DoH 直连，见 [`crate::util::dns_fallback_overrides`]）。
fn build_http(
    user_agent: &'static str,
    jar: &Arc<CookieJar>,
    hosts: &[(&str, u16)],
) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(crate::util::HTTP_TIMEOUT)
        .cookie_provider(jar.clone());
    for o in crate::util::dns_fallback_overrides(hosts) {
        builder = builder.resolve(&o.host, o.addr);
    }
    builder.build().expect("reqwest client build")
}

/// 生成验证码 uid（CAS 接受任意客户端提供的字符串，用 32 位 hex）。
fn random_uid() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..4)
        .map(|_| format!("{:08x}", rng.random::<u32>()))
        .collect()
}

/// CAS 登录端点路径。
const TICKETS_PATH: &str = "/v1/tickets";
/// 图形验证码端点路径。
const KAPTCHA_PATH: &str = "/kaptcha";
/// 教务侧验票入口（联奕签发的 service）。
pub const SSO_PATH: &str = "/sso/lyiotlogin";
/// 学生个人课表（正方 jwglxt），一次返回整学期。
const TIMETABLE_PATH: &str = "/jwglxt/kbcx/xskbcx_cxXsgrkb.html";
/// 课表功能码（正方固定值）。
const GNMKDM_KB: &str = "N2151";

/// 学期码：正方 xqm 编码。3=第一学期、12=第二学期、16=第三学期。
pub const TERM_CODES: [(&str, &str); 3] = [("3", "1"), ("12", "2"), ("16", "3")];

/// 课表行课条目（kbList 的业务子集）。
#[derive(Debug, Clone, PartialEq)]
pub struct CourseMeeting {
    pub course: String,
    pub course_id: String,
    pub teacher: String,
    pub classroom: String,
    /// 星期 1-7。
    pub weekday: u8,
    /// 原始节次文本，如 "9-10节"。
    pub sections_raw: String,
    pub start_section: Option<u8>,
    pub end_section: Option<u8>,
    /// 解析后的周次列表（zcd）。
    pub weeks: Vec<u32>,
    pub weeks_raw: String,
    pub class_type: String,
    pub class_group: String,
    pub xnm: String,
    pub xqm: String,
}

impl CourseMeeting {
    /// 稳定签名：变更检测的粒度（同一门课换教室/换周次即视为变更）。
    pub fn signature(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.course_id,
            self.weekday,
            self.sections_raw,
            self.weeks_raw,
            self.classroom,
            self.teacher
        )
    }
}

/// 课表响应（只取需要的字段；其余宽容忽略）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimetableResponse {
    #[serde(default)]
    kb_list: Vec<KbEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KbEntry {
    #[serde(default)]
    kcmc: Option<String>,
    #[serde(rename = "kch_id", default)]
    course_id: Option<String>,
    #[serde(default)]
    xm: Option<String>,
    #[serde(default)]
    xqj: Option<serde_json::Value>,
    #[serde(default)]
    jc: Option<String>,
    #[serde(default)]
    jcs: Option<String>,
    #[serde(default)]
    zcd: Option<String>,
    #[serde(default)]
    cdmc: Option<String>,
    #[serde(default)]
    kcxz: Option<String>,
    #[serde(default)]
    jxbmc: Option<String>,
    #[serde(default)]
    xnm: Option<serde_json::Value>,
    #[serde(default)]
    xqm: Option<serde_json::Value>,
}

/// 会话持久化文件结构：CAS TGT + 教务 cookie + 待验证的验证码 uid。
#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
struct PersistedSession {
    #[serde(default)]
    cas_tgt: Option<String>,
    #[serde(default)]
    cookies: Vec<PersistedCookie>,
    /// 已下载但未提交答案的验证码 uid（`jwxt login` 两步之间传递）。
    #[serde(default)]
    captcha_uid: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PersistedCookie {
    domain: String,
    name: String,
    value: String,
}

/// CAS 登录失败的具体形态，调用方据此决定提示语。
#[derive(Debug, thiserror::Error)]
pub enum JwxtError {
    /// 自动登录仍无法建立会话（验证码风格变化等）；手动兜底：
    /// `ru41 jwxt login` 下载验证码图，人工读数后 `--captcha <答案>` 提交。
    #[error(
        "自动登录失败：可手动运行 `ru41 jwxt login` 查看验证码后用 `--captcha <答案>` 完成登录"
    )]
    LoginRequired,
    /// CAS 拒绝（密码错误、账号锁定等）。
    #[error("CAS 登录被拒绝：{0}")]
    CasRejected(String),
    /// 其他网络/协议错误。
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// 最小 cookie 存储（同 chaoxing：按 (域名, 名字) 存值，可持久化）。
#[derive(Default)]
struct CookieJar {
    cookies: Mutex<HashMap<(String, String), String>>,
}

impl CookieJar {
    fn host_matches(domain: &str, host: &str) -> bool {
        host == domain || host.ends_with(&format!(".{domain}"))
    }
}

impl CookieStore for CookieJar {
    fn set_cookies(
        &self,
        cookie_headers: &mut dyn Iterator<Item = &reqwest::header::HeaderValue>,
        url: &url::Url,
    ) {
        let host = url.host_str().unwrap_or_default().to_owned();
        let mut jar = self.cookies.lock().unwrap();
        for raw in cookie_headers {
            let Ok(text) = raw.to_str() else { continue };
            let mut parts = text.split(';');
            let Some(kv) = parts.next() else { continue };
            let Some((name, value)) = kv.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            let domain = parts
                .filter_map(|attr| {
                    let attr = attr.trim();
                    attr.strip_prefix("domain=")
                        .or_else(|| attr.strip_prefix("Domain="))
                })
                .map(|d| d.trim_start_matches('.').to_owned())
                .next()
                .unwrap_or(host.clone());
            jar.insert((domain, name.to_owned()), value.trim().to_owned());
        }
    }

    fn cookies(&self, url: &url::Url) -> Option<reqwest::header::HeaderValue> {
        let host = url.host_str().unwrap_or_default();
        let jar = self.cookies.lock().unwrap();
        let pairs: Vec<String> = jar
            .iter()
            .filter(|((domain, _), value)| Self::host_matches(domain, host) && !value.is_empty())
            .map(|((_, name), value)| format!("{name}={value}"))
            .collect();
        if pairs.is_empty() {
            return None;
        }
        reqwest::header::HeaderValue::from_str(&pairs.join("; ")).ok()
    }
}

/// 教务协议客户端。
pub struct JwxtClient {
    jar: Arc<CookieJar>,
    http: reqwest::Client,
    username: String,
    password: String,
    /// CAS 基址（联奕 lyuapServer），生产默认 https://cas.gzus.edu.cn/lyuapServer。
    cas_base: String,
    /// Ohdave RSA 公钥参数（校方 CAS 前端配置，只经本地配置覆盖）。
    cas_modulus: String,
    cas_exponent: String,
    /// 教务基址，生产默认 https://jwxt.gzus.edu.cn。
    jwxt_base: String,
    session_path: std::path::PathBuf,
}

impl JwxtClient {
    pub fn new(
        username: impl Into<String>,
        password: impl Into<String>,
        cas_base: String,
        cas_modulus: String,
        cas_exponent: String,
        jwxt_base: String,
        session_path: std::path::PathBuf,
    ) -> Self {
        let jar = Arc::new(CookieJar::default());
        let cas_host = host_of(&cas_base);
        let jwxt_host = host_of(&jwxt_base);
        let http = build_http(
            concat!("ru41/", env!("CARGO_PKG_VERSION")),
            &jar,
            &[(&cas_host, 443), (&jwxt_host, 443)],
        );
        Self {
            jar,
            http,
            username: username.into(),
            password: password.into(),
            cas_base,
            cas_modulus,
            cas_exponent,
            jwxt_base,
            session_path,
        }
    }

    /// 测试用：浏览器 UA（部分校园中间件对非浏览器 UA 敏感）。
    pub fn with_browser_ua(mut self) -> Self {
        let cas_host = host_of(&self.cas_base);
        let jwxt_host = host_of(&self.jwxt_base);
        self.http = build_http(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
            &self.jar,
            &[(&cas_host, 443), (&jwxt_host, 443)],
        );
        self
    }

    fn load_session(&self) -> PersistedSession {
        let Ok(raw) = std::fs::read_to_string(&self.session_path) else {
            return PersistedSession::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    fn save_session(&self, session: &PersistedSession) -> Result<()> {
        if let Some(parent) = self.session_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
        let tmp = self.session_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string(session)?)?;
        std::fs::rename(&tmp, &self.session_path)?;
        Ok(())
    }

    fn persist_tgt(&self, tgt: &str) -> Result<()> {
        let mut session = self.load_session();
        session.cas_tgt = Some(tgt.to_owned());
        session.cookies = self
            .jar
            .cookies
            .lock()
            .unwrap()
            .iter()
            .map(|((domain, name), value)| PersistedCookie {
                domain: domain.clone(),
                name: name.clone(),
                value: value.clone(),
            })
            .collect();
        self.save_session(&session)
    }

    /// 生成图形验证码并保存 PNG，返回 (uid, 图片路径)。
    /// 验证码与 uid 绑定：uid 持久化到会话文件，`jwxt login --captcha` 用同一
    /// uid 提交答案。答案大小写按原样提交。
    pub async fn download_captcha(&self) -> Result<(String, std::path::PathBuf)> {
        let requested = random_uid();
        #[derive(Deserialize)]
        struct Kaptcha {
            #[serde(default)]
            uid: Option<String>,
            #[serde(default)]
            content: Option<String>,
        }
        let kaptcha: Kaptcha = self
            .http
            .get(format!("{}{KAPTCHA_PATH}", self.cas_base))
            .query(&[("uid", requested.as_str())])
            .send()
            .await
            .context("获取 CAS 验证码失败")?
            .json()
            .await
            .context("CAS 验证码响应不是合法 JSON")?;
        let data_url = kaptcha.content.context("CAS 验证码响应缺少 content")?;
        let png = data_url
            .split_once(',')
            .map(|(_, b64)| b64)
            .unwrap_or(&data_url);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(png)
            .context("CAS 验证码 base64 解码失败")?;
        let path = self.session_path.with_extension("captcha.png");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
        std::fs::write(&path, &bytes)
            .with_context(|| format!("写入验证码失败: {}", path.display()))?;
        let uid = kaptcha.uid.unwrap_or(requested);
        let mut session = self.load_session();
        session.captcha_uid = Some(uid.clone());
        self.save_session(&session)?;
        Ok((uid, path))
    }

    /// `jwxt login --captcha` 用：上次下载的验证码 uid。
    pub fn pending_captcha_uid(&self) -> Option<String> {
        self.load_session().captcha_uid
    }

    /// CAS 账密登录（需验证码答案），成功后持久化 TGT。
    pub async fn cas_login(&self, captcha_uid: &str, captcha_code: &str) -> Result<()> {
        #[derive(Deserialize, Default)]
        struct TicketsResponse {
            #[serde(default)]
            tgt: Option<String>,
            #[serde(default)]
            data: Option<serde_json::Value>,
        }
        let response: TicketsResponse = self
            .http
            .post(format!("{}{TICKETS_PATH}", self.cas_base))
            .form(&[
                ("username", self.username.as_str()),
                (
                    "password",
                    ohdave_encrypt(&self.password, &self.cas_exponent, &self.cas_modulus).as_str(),
                ),
                ("service", format!("{}{SSO_PATH}", self.jwxt_base).as_str()),
                ("loginType", ""),
                ("id", captcha_uid),
                ("code", captcha_code),
            ])
            .send()
            .await
            .context("CAS 登录请求失败")?
            .json()
            .await
            .context("CAS 登录响应不是合法 JSON")?;
        if let Some(tgt) = response.tgt.filter(|t| t.starts_with("TGT-")) {
            self.persist_tgt(&tgt)?;
            return Ok(());
        }
        let code = response
            .data
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("UNKNOWN");
        bail!("CAS 登录被拒绝（{code}）：请检查密码或重新输入验证码")
    }

    /// 用 TGT 换 ST 并建立教务会话；成功后持久化会话 cookie。
    /// SSO 验票端点有防抖/限流（实测连续验票会 404 甚至超时），重试必须换
    /// 新 ST（失败的那张可能已被烧掉），间隔 1s，最多 2 次。
    async fn establish_session(&self, tgt: &str) -> Result<()> {
        let mut last_err = anyhow::anyhow!("未尝试");
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            match self.establish_session_once(tgt).await {
                Ok(()) => return Ok(()),
                Err(err) => last_err = err,
            }
        }
        Err(last_err)
    }

    async fn establish_session_once(&self, tgt: &str) -> Result<()> {
        let st = self
            .http
            .post(format!(
                "{}{TICKETS_PATH}/{}",
                self.cas_base,
                tgt.trim_start_matches('/')
            ))
            .form(&[
                ("service", format!("{}{SSO_PATH}", self.jwxt_base).as_str()),
                ("loginToken", "loginToken"),
            ])
            .send()
            .await
            .context("CAS ST 换取失败")?
            .text()
            .await
            .context("CAS ST 响应读取失败")?;
        let st = st.trim();
        anyhow::ensure!(
            st.starts_with("ST-") || st.starts_with("http"),
            "CAS ST 响应异常：{st}"
        );
        let st_url = if st.starts_with("http") {
            st.to_owned()
        } else {
            format!("{}{SSO_PATH}?ticket={st}", self.jwxt_base)
        };
        // 跟随教务 302 链直到落回 index（会话 cookie 在链上各跳中写入）
        let response = self
            .http
            .get(&st_url)
            .send()
            .await
            .context("教务验票请求失败")?;
        let final_url = response.url().clone();
        let status = response.status();
        let _ = response
            .error_for_status()
            .with_context(|| format!("教务验票跳转失败：{status} @ {final_url}"))?;
        if !final_url.as_str().contains("login_slogin") {
            self.persist_tgt(tgt)?;
            return Ok(());
        }
        bail!("教务验票后仍停留在登录页，会话未建立")
    }

    /// 确保教务会话可用：持久化 cookie 有效则复用，其次 TGT 换新 ST；
    /// 都不行时返回 [`JwxtError::LoginRequired`]（需要人工验证码登录）。
    /// 自动登录：下载验证码 → 本地求解（算术题模板匹配）→ 提交。
    /// 验证码识别失败或答案被拒（CODEFALSE）时换新图重试，最多 3 张；
    /// 密码错误/账号锁定等不可恢复错误立即返回（不重试，防触发风控）。
    pub async fn cas_login_auto(&self) -> Result<()> {
        let mut last_err: Option<anyhow::Error> = None;
        for _ in 0..3 {
            let (uid, path) = self.download_captcha().await?;
            let png = std::fs::read(&path).context("读取验证码图片失败")?;
            let Some(answer) = super::captcha::solve(&png) else {
                continue; // 认不出这张图，换新的一张
            };
            match self.cas_login(&uid, &answer).await {
                Ok(()) => return Ok(()),
                Err(err) if err.to_string().contains("CODEFALSE") => {
                    last_err = Some(err); // 识别错了，换图重试
                }
                Err(err) => return Err(err),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("验证码识别连续失败，请检查模板库")))
    }

    /// 确保教务会话可用：持久化 cookie 有效则复用；否则 TGT 换新 ST；
    /// TGT 缺失/失效时自动完成验证码登录（算术题本地求解，无需人工）。
    /// 只有密码错误/账号锁定等硬失败才返回错误，由调用方提醒。
    pub async fn ensure_session(&self) -> std::result::Result<(), JwxtError> {
        if self.session_valid().await {
            return Ok(());
        }
        if let Some(tgt) = self.load_session().cas_tgt {
            if self.establish_session(&tgt).await.is_ok() && self.session_valid().await {
                return Ok(());
            }
            // TGT 失效：清除本地记录，转自动登录
            let mut session = self.load_session();
            session.cas_tgt = None;
            let _ = self.save_session(&session);
        }
        self.cas_login_auto().await.map_err(|err| {
            let text = err.to_string();
            if text.contains("PASSERROR") || text.contains("USERLOCK") {
                JwxtError::CasRejected(text)
            } else {
                JwxtError::Other(err)
            }
        })?;
        let Some(tgt) = self.load_session().cas_tgt else {
            return Err(JwxtError::LoginRequired);
        };
        self.establish_session(&tgt)
            .await
            .map_err(JwxtError::Other)?;
        if self.session_valid().await {
            Ok(())
        } else {
            Err(JwxtError::LoginRequired)
        }
    }

    /// 教务会话是否有效（课表接口返回 JSON 而非跳登录页）。
    /// 探测用的学期参数不影响判定：任何会话下该接口都返回 JSON（可能空表）。
    async fn session_valid(&self) -> bool {
        matches!(self.timetable_raw("2026", "3").await, Ok(Some(_)))
    }

    /// 课表原始响应；会话有效时返回 Some(JSON 文本)，被踢回登录页返回 None。
    async fn timetable_raw(&self, xnm: &str, xqm: &str) -> Result<Option<String>> {
        let response = self
            .http
            .post(format!("{}{TIMETABLE_PATH}", self.jwxt_base))
            .query(&[("gnmkdm", GNMKDM_KB)])
            .form(&[("xnm", xnm), ("xqm", xqm), ("gnmkdm", GNMKDM_KB)])
            .send()
            .await
            .context("课表请求失败")?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let text = response.text().await.context("课表响应读取失败")?;
        if content_type.contains("json") {
            Ok(Some(text))
        } else {
            // 302 跟随后落到登录页 HTML：会话失效
            Ok(None)
        }
    }

    /// 拉取并解析整学期课表。会话失效时返回 [`JwxtError::LoginRequired`]。
    pub async fn timetable(
        &self,
        xnm: &str,
        xqm: &str,
    ) -> std::result::Result<Vec<CourseMeeting>, JwxtError> {
        let Some(raw) = self
            .timetable_raw(xnm, xqm)
            .await
            .map_err(JwxtError::Other)?
        else {
            self.ensure_session().await?;
            let raw = self
                .timetable_raw(xnm, xqm)
                .await
                .map_err(JwxtError::Other)?
                .context("重试后仍未取到课表（登录态失效）")
                .map_err(JwxtError::Other)?;
            return decode_timetable(&raw);
        };
        decode_timetable(&raw)
    }
}

/// 解析课表 JSON（kbList → CourseMeeting；字段缺失/异常行跳过不致命）。
pub fn decode_timetable(raw: &str) -> std::result::Result<Vec<CourseMeeting>, JwxtError> {
    let response: TimetableResponse =
        serde_json::from_str(raw).map_err(|err| JwxtError::Other(anyhow::anyhow!(err)))?;
    let mut meetings = Vec::new();
    for entry in response.kb_list {
        let Some(course) = entry.kcmc else { continue };
        let weekday = match entry.xqj.as_ref().and_then(value_as_u8) {
            Some(day @ 1..=7) => day,
            _ => continue,
        };
        let sections_raw = entry.jc.or(entry.jcs).unwrap_or_default();
        let (start_section, end_section) = parse_sections(&sections_raw);
        let weeks_raw = entry.zcd.unwrap_or_default();
        meetings.push(CourseMeeting {
            course,
            course_id: entry.course_id.unwrap_or_default(),
            teacher: entry.xm.unwrap_or_default(),
            classroom: entry.cdmc.unwrap_or_default(),
            weekday,
            start_section,
            end_section,
            weeks: parse_weeks(&weeks_raw),
            sections_raw,
            weeks_raw,
            class_type: entry.kcxz.unwrap_or_default(),
            class_group: entry.jxbmc.unwrap_or_default(),
            xnm: entry
                .xnm
                .and_then(|v| value_as_string(&v))
                .unwrap_or_default(),
            xqm: entry
                .xqm
                .and_then(|v| value_as_string(&v))
                .unwrap_or_default(),
        });
    }
    Ok(meetings)
}

fn value_as_u8(value: &serde_json::Value) -> Option<u8> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())?
        .try_into()
        .ok()
}

fn value_as_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        other if !other.is_null() => Some(other.to_string()),
        _ => None,
    }
}

/// 解析节次文本："9-10节" / "9-10" / "13-16节" → (起始节, 结束节)。
pub fn parse_sections(text: &str) -> (Option<u8>, Option<u8>) {
    let cleaned: String = text
        .chars()
        .map(|c| match c {
            '－' | '—' | '–' | '～' | '~' => '-',
            '，' | '、' => ',',
            other => other,
        })
        .collect();
    let digits: String = cleaned
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-' || *c == ',')
        .collect();
    let mut parts = digits.split(['-', ',']).filter(|p| !p.is_empty());
    let start = parts.next().and_then(|p| p.parse().ok());
    let end = parts.next().and_then(|p| p.parse().ok());
    match (start, end) {
        (Some(s), Some(e)) => (Some(s), Some(e)),
        (Some(s), None) => (Some(s), Some(s)),
        _ => (None, None),
    }
}

/// 解析周次文本："第3周" → [3]；"4-19周" → 4..=19；"1,3,5周" → [1,3,5]。
/// 无法识别的部分跳过；返回升序列表。
pub fn parse_weeks(text: &str) -> Vec<u32> {
    let cleaned: String = text
        .chars()
        .map(|c| match c {
            '－' | '—' | '–' | '～' | '~' => '-',
            '，' | '、' => ',',
            other => other,
        })
        .collect();
    let mut weeks = Vec::new();
    // 逗号（含全角）视作分隔符；连字符在片段内表达范围
    for token in cleaned.split(|c: char| !(c.is_ascii_digit() || c == '-')) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if let Some((start, end)) = token.split_once('-')
            && let (Ok(s), Ok(e)) = (start.parse::<u32>(), end.parse::<u32>())
        {
            weeks.extend(s..=e);
            continue;
        }
        if let Ok(week) = token.parse::<u32>() {
            weeks.push(week);
        }
    }
    weeks.sort_unstable();
    weeks.dedup();
    weeks
}

/// Ohdave BigInt RSA（联奕 CAS 前端加密，语义经页面 JS 逐字节对照）。
/// 密码按 chunkSize = 2×h(modulus) 字符分块，每块小端打包后 modPow；
/// 输出按 16-bit limb 自高到低每组 4 位 hex，多块以单空格连接。
/// 仅推荐 ASCII 密码（非 ASCII 字符的行为在原 JS 中即未定义）。
pub fn ohdave_encrypt(text: &str, exponent_hex: &str, modulus_hex: &str) -> String {
    let exponent = BigUint::from_str_radix(exponent_hex, 16).expect("invalid RSA exponent");
    let modulus = BigUint::from_str_radix(modulus_hex, 16).expect("invalid RSA modulus");
    // h(modulus)：最高非零 16-bit digit 下标（4 个 hex 字符一组，自高位起）
    let groups: Vec<u16> = modulus_hex
        .as_bytes()
        .chunks(4)
        .map(|chunk| {
            u16::from_str_radix(std::str::from_utf8(chunk).unwrap_or("0"), 16).unwrap_or(0)
        })
        .collect();
    let highest = groups.iter().rposition(|g| *g != 0).unwrap_or(0);
    let chunk = 2 * highest;

    let codes: Vec<u32> = text.chars().map(|c| c as u32).collect();
    let mut blocks = Vec::new();
    for offset in (0..codes.len()).step_by(chunk.max(1)) {
        let slice = &codes[offset..(offset + chunk).min(codes.len())];
        let mut bytes = vec![0u8; chunk];
        for (index, code) in slice.iter().enumerate() {
            bytes[index] = (*code & 0xFF) as u8;
        }
        let m = BigUint::from_bytes_le(&bytes);
        let c = m.modpow(&exponent, &modulus);
        blocks.push(limb_hex(&c));
    }
    blocks.join(" ")
}

/// BigUint → 按 16-bit limb 自高到低的 4 位 hex（每 limb 恒 4 字符，宽度 =
/// (最高非零 limb 下标+1)×4），对齐 Ohdave u(m) 的输出。
fn limb_hex(value: &BigUint) -> String {
    if value.is_zero() {
        return "0000".to_owned();
    }
    let bytes = value.to_bytes_le();
    let mut limbs = Vec::new();
    for chunk in bytes.chunks(2) {
        let low = chunk[0] as u16;
        let high = chunk.get(1).map(|b| (*b as u16) << 8).unwrap_or(0);
        limbs.push(low | high);
    }
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
    limbs
        .iter()
        .rev()
        .map(|limb| format!("{limb:04x}"))
        .collect()
}

/// 校方 CAS 前端配置的 RSA 公钥（与浏览器内 JS 同源，密文可被服务端解密）。
pub const CAS_TEST_MODULUS: &str = "00b5eeb166e069920e80bebd1fea4829d3d1f3216f2aabe79b6c47a3c18dcee5fd22c2e7ac519cab59198ece036dcf289ea8201e2a0b9ded307f8fb704136eaeb670286f5ad44e691005ba9ea5af04ada5367cd724b5a26fdb5120cc95b6431604bd219c6b7d83a6f8f24b43918ea988a76f93c333aa5a20991493d4eb1117e7b1";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::jwxt::infer_term;

    /// 与校方 CAS 前端 JS（node 执行同源 module）的输出逐字节一致。
    /// 这两个向量由真实页面代码生成，改动加密实现必须保持它们通过。
    #[test]
    fn ohdave_encrypt_matches_frontend_vectors() {
        assert_eq!(
            ohdave_encrypt("TESTPASS123", "010001", CAS_TEST_MODULUS),
            "574f115af2fb253cc526e8cf990330391d9d1a91175839e77e6a7ca290318ea0e3dec3d3141c15dce4875b9b613df1b034e6bf66ab3a2e399d90b08abf5cfeb20e58a4460334b8413a8529741e1cf9ada3c1673fd4e6e6a33ff7871b91e0e3d275086469dab45b912a5b7236023acc0b9009a31d30872fb5169293b4217dd545"
        );
        assert_eq!(
            ohdave_encrypt("abc123", "010001", CAS_TEST_MODULUS),
            "754a54d655d2c24a649ee9cba2e62348dffde8493adcfe7b78a9b3d499c7579b74013e5eda83b580f99a6060583ae9d57fba94b8a90496338a4fb3becb289194844d892f3e8af62f83f3e195ffe7b134b3dbc4a2c87cfe3f41b3e6436c1fdc76b696ddcd8bc1e5c5007784dbb87864bb0292859a5977997c470720b523387728"
        );
    }

    #[test]
    fn parse_sections_handles_common_forms() {
        assert_eq!(parse_sections("9-10节"), (Some(9), Some(10)));
        assert_eq!(parse_sections("13-16节"), (Some(13), Some(16)));
        assert_eq!(parse_sections("5"), (Some(5), Some(5)));
        assert_eq!(parse_sections("1-2节"), (Some(1), Some(2)));
        assert_eq!(parse_sections("第N节"), (None, None));
    }

    #[test]
    fn parse_weeks_handles_ranges_and_lists() {
        assert_eq!(parse_weeks("第3周"), vec![3]);
        assert_eq!(parse_weeks("4-19周"), (4..=19).collect::<Vec<_>>());
        assert_eq!(parse_weeks("1,3,5周"), vec![1, 3, 5]);
        assert_eq!(parse_weeks("10-18周"), (10..=18).collect::<Vec<_>>());
        assert!(parse_weeks("无").is_empty());
    }

    #[test]
    fn decode_timetable_parses_kb_list() {
        let raw = r#"{"qsxqj":1,"xsxx":{"XNM":"2026"},"kbList":[
            {"kcmc":"军事理论","kch_id":"ABC123","xm":"鲁鲜亮","xqj":1,"jc":"9-10节",
             "jcs":"9-10","zcd":"第3周","cdmc":"T301","kcxz":"必修","jxbmc":"(2026-2027-1)-GE1062-08","xnm":"2026","xqm":"3"},
            {"kcmc":"缺星期行","xqj":"x"},
            {"kcmc":"范围周次","kch_id":"DEF","xm":"刘丹","xqj":4,"jc":"13-16节","zcd":"4-19周","cdmc":"B101","xnm":"2026","xqm":"3"}
        ]}"#;
        let meetings = decode_timetable(raw).expect("decode");
        assert_eq!(meetings.len(), 2);
        assert_eq!(meetings[0].course, "军事理论");
        assert_eq!(meetings[0].weekday, 1);
        assert_eq!(meetings[0].start_section, Some(9));
        assert_eq!(meetings[0].weeks, vec![3]);
        assert_eq!(meetings[0].xnm, "2026");
        assert_eq!(meetings[1].weeks.len(), 16);
        // 签名稳定：同字段不同顺序的字段集应产出同一签名
        assert_eq!(meetings[0].signature(), "ABC123|1|9-10节|第3周|T301|鲁鲜亮");
    }

    #[test]
    fn infer_term_follows_semester_calendar() {
        use chrono::NaiveDate;
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        assert_eq!(infer_term(d(10, 20)), ("2026".to_owned(), "3".to_owned()));
        assert_eq!(infer_term(d(1, 15)), ("2025".to_owned(), "3".to_owned()));
        assert_eq!(infer_term(d(3, 1)), ("2025".to_owned(), "12".to_owned()));
        assert_eq!(infer_term(d(6, 30)), ("2025".to_owned(), "12".to_owned()));
    }
}
