//! 移动图书馆（libsp 系）协议客户端：联奕 CAS 账密登录 → 超星统一认证验票 →
//! libsp JWT 会话 → 借阅查询/续借。协议调研与抓包记录见 docs/library-recon.md。
//!
//! 登录链（四跳，全部纯 HTTP）：
//! 1. `POST {cas}/v1/tickets`（Ohdave RSA 密码 + 算术验证码，模板复用
//!    `ru41::modules::jwxt::captcha`）→ TGT；
//! 2. `POST {cas}/v1/tickets/<TGT>` 换 ST（service = 超星统一认证）；
//! 3. `GET {unified}/login_auth/v2/cas/gzus/index?ticket=<ST>` → 页面内嵌
//!    AJAX 参数 data/time/enc → `GET .../gzus/login?...` 建立超星会话；
//! 4. `GET {find}/find/sso/login/gzus/1?data&time&enc` → 302 落地 URL 携带
//!    `jwt=<JWT>&jwtHeader=<头名>`（头名动态读，JWT 有效期 24h）。
//!
//! 会话（TGT + cookie + JWT）持久化到本地文件；JWT 过期前直接复用，
//! 过期后自动重走全链。CAS 密码错（PASSERROR）/锁号（USERLOCK）立即停止
//! 重试——满 5 次锁号，不能每 poll 烧一次。

use anyhow::{Context, Result, bail};
use base64::Engine;
use rand::Rng;
use regex::Regex;
use reqwest::cookie::CookieStore;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 业务 API 相对路径：当前借阅。
pub const LOAN_LIST_PATH: &str = "/find/loanInfo/loanList";
/// 业务 API 相对路径：借阅历史。
pub const LOAN_HISTORY_PATH: &str = "/find/loanInfo/loanHistoryList";
/// 业务 API 相对路径：续借。
pub const RENEW_PATH: &str = "/find/lendbook/reNew";

/// 借阅/历史记录的业务字段子集（接口宽容解析，缺字段为 None）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoanRecord {
    pub title: Option<String>,
    pub author: Option<String>,
    pub isbn: Option<String>,
    /// 借出日期。
    pub loan_date: Option<String>,
    /// ★ 应还日期（催还提醒的基准）。
    pub norm_return_date: Option<String>,
    /// 实还日期（历史记录里有；在借为空）。
    pub return_date: Option<String>,
    /// 馆藏地（如「计算机书库」）。
    pub location_name: Option<String>,
    pub barcode: Option<String>,
    pub prop_no: Option<String>,
    pub loan_id: Option<String>,
    pub record_id: Option<String>,
}

/// 一次 loanList / loanHistoryList 的结果。
#[derive(Debug, Clone, Default)]
pub struct LoanPage {
    pub records: Vec<LoanRecord>,
    /// 还可借册数（loanList 才有）。
    pub loan_num: Option<i64>,
    /// 当前在借/命中数。
    pub num_found: Option<i64>,
}

/// PC 统一检索结果条目（find/unify/search）。
#[derive(Clone, Debug, Default)]
pub struct SearchBook {
    pub record_id: i64,
    pub title: String,
    pub author: Option<String>,
    pub publisher: Option<String>,
    pub publish_year: Option<String>,
    pub isbn: Option<String>,
    /// 索书号（callNoOne，或 callNo[0]）。
    pub call_no: Option<String>,
    /// 馆藏数。
    pub physical_count: i64,
}

/// 检索结果页。
pub struct SearchPage {
    pub num_found: i64,
    pub books: Vec<SearchBook>,
}

/// 单册馆藏（find/physical/groupitems）。
#[derive(Clone, Debug, Default)]
pub struct HoldingItem {
    pub call_no: Option<String>,
    pub barcode: Option<String>,
    pub location_name: Option<String>,
    pub cur_location_name: Option<String>,
    /// 在架 / 借出 等。
    pub process_type: Option<String>,
}

impl HoldingItem {
    /// 是否可借（在架）。
    pub fn is_on_shelf(&self) -> bool {
        self.process_type.as_deref() == Some("在架")
    }

    /// 样本库单册：仅供馆内阅览，不外借（字段与普通馆藏一致，
    /// 只能靠馆藏地名字识别，如「广州校区样本书库」）。
    pub fn is_sample(&self) -> bool {
        self.location_name
            .as_deref()
            .is_some_and(|l| l.contains("样本"))
    }
}

impl LoanRecord {
    /// 提醒与按钮用的去重键：优先 recordId/loanId/barcode，都没有就用书名+应还日。
    pub fn key(&self) -> String {
        self.record_id
            .clone()
            .or_else(|| self.loan_id.clone())
            .or_else(|| self.barcode.clone())
            .or_else(|| self.prop_no.clone())
            .unwrap_or_else(|| {
                format!(
                    "{}|{}",
                    self.title.as_deref().unwrap_or(""),
                    self.norm_return_date.as_deref().unwrap_or("")
                )
            })
    }

    /// 展示书名（缺书名时用条码兜底）。
    pub fn display_title(&self) -> String {
        self.title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("条码 {}", self.barcode.as_deref().unwrap_or("?")))
    }

    /// 续借请求的自适应参数集：reNew 的确切请求体未抓包验证（当前无在借书），
    /// 把记录里所有可能的定位字段一起带上，由服务端「参数异常 {0}」提示补齐。
    pub fn renew_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "barcode": self.barcode,
            "propNo": self.prop_no,
            "loanId": self.loan_id,
            "recordId": self.record_id,
        })
    }
}

/// 会话持久化文件结构：CAS TGT + 各域 cookie + libsp JWT 与鉴权头名。
#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
struct PersistedSession {
    #[serde(default)]
    cas_tgt: Option<String>,
    #[serde(default)]
    cookies: Vec<PersistedCookie>,
    #[serde(default)]
    jwt: Option<String>,
    /// 鉴权头名，从落地 URL 的 jwtHeader 参数读取（可被校方更换）。
    #[serde(default)]
    jwt_header: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PersistedCookie {
    domain: String,
    name: String,
    value: String,
}

/// 最小 cookie 存储（同 jwxt：按 (域名, 名字) 存值，可持久化）。
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

/// 从基址 URL 提取主机名（DoH 兜底按主机注入 resolve 表）。
fn host_of(base: &str) -> String {
    url::Url::parse(base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

fn build_http(user_agent: &'static str, jar: &Arc<CookieJar>, hosts: &[(&str, u16)]) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(ru41::util::HTTP_TIMEOUT)
        .cookie_provider(jar.clone());
    for o in ru41::util::dns_fallback_overrides(hosts) {
        builder = builder.resolve(&o.host, o.addr);
    }
    builder.build().expect("reqwest client build")
}

/// 调试用：不跟随重定向的客户端（手动逐跳检查 SSO 响应）。
fn build_http_noredir(
    user_agent: &'static str,
    jar: &Arc<CookieJar>,
    hosts: &[(&str, u16)],
) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(ru41::util::HTTP_TIMEOUT)
        .cookie_provider(jar.clone())
        .redirect(reqwest::redirect::Policy::none());
    for o in ru41::util::dns_fallback_overrides(hosts) {
        builder = builder.resolve(&o.host, o.addr);
    }
    builder.build().expect("reqwest client build")
}

/// find 公开检索专用客户端：不带 cookie jar（带 oga SESSION 会被当登录态 → 401）。
fn build_http_public(hosts: &[(&str, u16)]) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
        )
        .timeout(ru41::util::HTTP_TIMEOUT);
    for o in ru41::util::dns_fallback_overrides(hosts) {
        builder = builder.resolve(&o.host, o.addr);
    }
    builder.build().expect("reqwest client build")
}

fn random_uid() -> String {
    let mut rng = rand::rng();
    (0..4).map(|_| format!("{:08x}", rng.random::<u32>())).collect()
}

/// 从验票页 HTML 提取内嵌 AJAX 参数（var data/time/enc）。
fn extract_sso_params(html: &str) -> Option<(String, String, String)> {
    let grab = |name: &str| -> Option<String> {
        let re = Regex::new(&format!(r#"var\s+{name}\s*=\s*"([^"]+)""#)).ok()?;
        Some(re.captures(html)?.get(1)?.as_str().to_owned())
    };
    let data = grab("data")?;
    let time = grab("time")?;
    let enc = grab("enc")?;
    Some((data, time, enc))
}

/// 从落地 URL 字符串提取 jwt 与鉴权头名（jwt 在 fragment 里也无所谓，整体扫描）。
fn extract_jwt(final_url: &str) -> Option<(String, String)> {
    let grab = |name: &str| -> Option<String> {
        let start = final_url.find(&format!("{name}="))? + name.len() + 1;
        let rest = &final_url[start..];
        let end = rest.find(['&', '#']).unwrap_or(rest.len());
        let value = &rest[..end];
        (!value.is_empty()).then(|| value.to_owned())
    };
    let jwt = grab("jwt")?;
    let header = grab("jwtHeader").unwrap_or_else(|| "jwtOpacAuth".into());
    Some((jwt, header))
}

/// JWT 剩余有效期（秒）；解析失败返回 None（视为不可信）。
pub fn jwt_ttl(jwt: &str) -> Option<i64> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let exp = value.get("exp")?.as_i64()?;
    Some(exp - ru41::util::now())
}

/// 图书馆协议客户端。base 地址全部可配置（生产用默认值，测试指到本地 mock）。
#[derive(Clone)]
pub struct LibraryClient {
    jar: Arc<CookieJar>,
    http: reqwest::Client,
    http_noredir: reqwest::Client,
    /// find 公开检索专用：不带 cookie（带 oga SESSION 会被当登录态 → 401）。
    http_public: reqwest::Client,
    username: String,
    password: String,
    cas_base: String,
    cas_modulus: String,
    cas_exponent: String,
    unified_base: String,
    find_base: String,
    mobile_base: String,
    /// find 站 mappingPath（"findgzus.libsp.cn" → "gzus"）。
    find_mapping_path: String,
    group_code: String,
    session_path: std::path::PathBuf,
}

impl LibraryClient {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        username: impl Into<String>,
        password: impl Into<String>,
        cas_base: String,
        cas_modulus: String,
        cas_exponent: String,
        unified_base: String,
        find_base: String,
        mobile_base: String,
        group_code: String,
        session_path: std::path::PathBuf,
    ) -> Self {
        let jar = Arc::new(CookieJar::default());
        let cas_host = host_of(&cas_base);
        let unified_host = host_of(&unified_base);
        let find_host = host_of(&find_base);
        let mobile_host = host_of(&mobile_base);
        let hosts: Vec<(&str, u16)> = vec![
            (cas_host.as_str(), 443u16),
            (unified_host.as_str(), 443),
            (find_host.as_str(), 443),
            (mobile_host.as_str(), 443),
        ];
        let http = build_http(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
            &jar,
            &hosts,
        );
        let http_noredir = build_http_noredir(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
            &jar,
            &hosts,
        );
        let http_public = build_http_public(&hosts);
        // findgzus.libsp.cn → gzus；其他 libsp 系（libsp.com/libstar.cn/libsp.net）同理。
        let find_mapping_path = find_host
            .strip_prefix("find")
            .and_then(|rest| rest.split('.').next())
            .filter(|s| !s.is_empty())
            .unwrap_or("gzus")
            .to_owned();
        Self {
            jar,
            http,
            http_noredir,
            http_public,
            username: username.into(),
            password: password.into(),
            cas_base,
            cas_modulus,
            cas_exponent,
            unified_base,
            find_base,
            mobile_base,
            find_mapping_path,
            group_code,
            session_path,
        }
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

    fn persist_jwt(&self, jwt: &str, jwt_header: &str) -> Result<()> {
        let mut session = self.load_session();
        session.jwt = Some(jwt.to_owned());
        session.jwt_header = Some(jwt_header.to_owned());
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

    /// 确保可用会话：持久化 JWT 未过期则复用，否则重走完整登录链。
    pub async fn ensure_session(&self) -> Result<()> {
        let session = self.load_session();
        if let Some(jwt) = &session.jwt
            && jwt_ttl(jwt).is_some_and(|ttl| ttl > 300)
        {
            return Ok(());
        }
        self.full_login().await
    }

    /// 完整登录链：CAS 账密 → ST → 超星验票 → libsp JWT。
    async fn full_login(&self) -> Result<()> {
        // 1) 超星统一认证入口：种 cookie、声明 refer 上下文（失败不致命）。
        let _ = self
            .http
            .get(format!("{}/login_auth/v2/gzus/init", self.unified_base))
            .query(&[(
                "refer",
                format!("{}/find/sso/login/gzus/1", self.find_base),
            )])
            .send()
            .await;

        // 2) CAS 账密登录（自动验证码）→ TGT。
        let tgt = self.cas_login_auto().await?;

        // 3) TGT → ST（service 必须与第 4 步验票 URL 一致）。
        let service = format!("{}/login_auth/v2/cas/gzus/index", self.unified_base);
        let st = self
            .http
            .post(format!(
                "{}/v1/tickets/{}",
                self.cas_base,
                tgt.trim_start_matches('/')
            ))
            .form(&[("service", service.as_str()), ("loginToken", "loginToken")])
            .send()
            .await
            .context("CAS ST 换取失败")?
            .text()
            .await
            .context("CAS ST 响应读取失败")?;
        let st = st.trim();
        anyhow::ensure!(
            st.starts_with("ST-"),
            "CAS ST 响应异常：{}",
            &st[..st.len().min(120)]
        );

        // 4) 超星统一认证验票 → 页面内嵌 data/time/enc → 建立超星会话。
        let html = self
            .http
            .get(&service)
            .query(&[("ticket", st)])
            .header("Referer", format!("{}/", self.find_base))
            .send()
            .await
            .context("超星统一认证验票失败")?
            .text()
            .await
            .context("超星验票页读取失败")?;
        let (data, time, enc) = extract_sso_params(&html)
            .context("超星验票页缺少 data/time/enc 参数（页面结构可能变更）")?;

        // 5) libsp 落地 → 抓 JWT 与鉴权头名。
        // 必须手动逐跳跟随：jwt 在 302 Location 的 `#/Home?jwt=...` 片段里，
        // reqwest 自动重定向会剥掉 fragment（fragments 不随请求发送），
        // response.url() 永远拿不到 jwt。
        let mut current = format!(
            "{}/find/sso/login/gzus/1?data={}&time={}&enc={}",
            self.find_base,
            data.as_str(),
            time.as_str(),
            enc.as_str()
        );
        let mut hop = 0;
        let response = loop {
            hop += 1;
            let resp = self
                .http_noredir
                .get(&current)
                .send()
                .await
                .context("libsp SSO 落地失败")?;
            let status = resp.status();
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            match location {
                Some(next) if status.is_redirection() && hop < 6 => {
                    current = if next.starts_with("http") {
                        next
                    } else {
                        format!("{}{next}", self.find_base)
                    };
                }
                _ => break resp,
            }
        };
        let final_url = current.clone();
        let _ = response.error_for_status();
        let (jwt, jwt_header) = extract_jwt(&final_url)
            .with_context(|| format!("libsp 落地 URL 缺少 jwt 参数：{final_url}"))?;
        self.persist_jwt(&jwt, &jwt_header)?;

        // 6) 超星会话建立（library 业务不依赖，仅为保持与浏览器流程一致的完整链）。
        let login_status: serde_json::Value = self
            .http
            .get(format!(
                "{}/login_auth/v2/cas/gzus/login",
                self.unified_base
            ))
            .query(&[
                ("data", data.as_str()),
                ("time", time.as_str()),
                ("enc", enc.as_str()),
                ("displayName", ""),
                ("userRole", ""),
            ])
            .send()
            .await
            .context("超星会话建立请求失败")?
            .json()
            .await
            .context("超星会话建立响应不是 JSON")?;
        if login_status.get("status") != Some(&serde_json::Value::Bool(true)) {
            bail!("超星会话建立被拒：{login_status}");
        }
        Ok(())
    }

    /// CAS 自动登录：下载算术验证码 → 本地模板求解 → 提交；识别失败换图重试
    /// 最多 3 张。PASSERROR/USERLOCK 立即返回（不烧密码计数）。
    async fn cas_login_auto(&self) -> Result<String> {
        let mut last_err: Option<anyhow::Error> = None;
        for _ in 0..3 {
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
                .get(format!("{}/kaptcha", self.cas_base))
                .query(&[("uid", requested.as_str())])
                .send()
                .await
                .context("获取 CAS 验证码失败")?
                .json()
                .await
                .context("CAS 验证码响应不是合法 JSON")?;
            let data_url = kaptcha.content.context("CAS 验证码响应缺少 content")?;
            let png = base64::engine::general_purpose::STANDARD
                .decode(data_url.split_once(',').map(|(_, b64)| b64).unwrap_or(&data_url))
                .context("CAS 验证码 base64 解码失败")?;
            let Some(answer) = ru41::modules::jwxt::captcha::solve(&png) else {
                continue; // 认不出这张图，换新的一张
            };
            let uid = kaptcha.uid.unwrap_or(requested);
            match self.cas_login(&uid, &answer).await {
                Ok(tgt) => return Ok(tgt),
                Err(err) if err.to_string().contains("CODEFALSE") => {
                    last_err = Some(err); // 识别错了，换图重试
                }
                Err(err) => return Err(err),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("验证码识别连续失败，请检查模板库")))
    }

    /// CAS 账密 + 验证码提交，成功返回 TGT 并持久化。
    async fn cas_login(&self, captcha_uid: &str, captcha_code: &str) -> Result<String> {
        #[derive(Deserialize, Default)]
        struct TicketsResponse {
            #[serde(default)]
            tgt: Option<String>,
            #[serde(default)]
            data: Option<serde_json::Value>,
        }
        let service = format!("{}/login_auth/v2/cas/gzus/index", self.unified_base);
        let response: TicketsResponse = self
            .http
            .post(format!("{}/v1/tickets", self.cas_base))
            .form(&[
                ("username", self.username.as_str()),
                (
                    "password",
                    ru41::modules::jwxt::source::ohdave_encrypt(
                        &self.password,
                        &self.cas_exponent,
                        &self.cas_modulus,
                    )
                    .as_str(),
                ),
                ("service", service.as_str()),
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
            return Ok(tgt);
        }
        let code = response
            .data
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("UNKNOWN");
        bail!("CAS 登录被拒绝（{code}）：请检查密码或账号状态")
    }

    /// 业务 API 调用（三头：鉴权 JWT / groupCode / mappingPath 空）。
    /// 收到 9999「系统访问中断」自动重登一次再试。
    async fn api_post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let session = self.load_session();
        let Some(jwt) = session.jwt else {
            bail!("缺少 libsp 会话，请先触发登录");
        };
        let jwt_header = session
            .jwt_header
            .unwrap_or_else(|| "jwtOpacAuth".into());
        let send = |jwt: String, jwt_header: String| {
            let url = format!("{}{path}", self.mobile_base);
            let mut request = self
                .http
                .post(&url)
                .header("groupCode", self.group_code.as_str())
                .header("mappingPath", "")
                .json(&body);
            request = if jwt_header.is_empty() {
                request
            } else {
                request.header(jwt_header.as_str(), jwt.as_str())
            };
            async move { request.send().await }
        };
        let response = send(jwt.clone(), jwt_header.clone())
            .await
            .with_context(|| format!("library API 请求失败: {path}"))?;
        let value: serde_json::Value = response
            .json()
            .await
            .with_context(|| format!("library API 响应不是 JSON: {path}"))?;
        if value.get("errCode").and_then(serde_json::Value::as_i64) == Some(9999) {
            // JWT 被服务端作废（提前失效）→ 重走登录链再试一次。
            self.full_login().await?;
            let session = self.load_session();
            let jwt = session.jwt.context("重登后仍缺少 libsp JWT")?;
            let jwt_header = session
                .jwt_header
                .unwrap_or_else(|| "jwtOpacAuth".into());
            let response = send(jwt, jwt_header)
                .await
                .with_context(|| format!("library API 重试失败: {path}"))?;
            return response
                .json()
                .await
                .with_context(|| format!("library API 重试响应不是 JSON: {path}"));
        }
        Ok(value)
    }

    fn parse_loan_page(value: serde_json::Value) -> Result<LoanPage> {
        if value.get("success") != Some(&serde_json::Value::Bool(true)) {
            bail!(
                "library API 被拒：{}",
                value
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
            );
        }
        let data = value.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let mut records = Vec::new();
        if let Some(list) = data
            .get("searchResult")
            .and_then(serde_json::Value::as_array)
        {
            for item in list {
                records.push(parse_record(item));
            }
        }
        Ok(LoanPage {
            records,
            loan_num: data.get("loanNum").and_then(serde_json::Value::as_i64),
            num_found: data.get("numFound").and_then(serde_json::Value::as_i64),
        })
    }

    /// 当前借阅列表（含应还日期）。
    pub async fn loans(&self) -> Result<LoanPage> {
        let body = serde_json::json!({
            "page": 1,
            "rows": 50,
            "searchType": 1,
            "searchContent": "",
            "sortType": 0,
            "startDate": serde_json::Value::Null,
            "endDate": serde_json::Value::Null,
        });
        Self::parse_loan_page(self.api_post(LOAN_LIST_PATH, body).await?)
    }

    /// 借阅历史（最近 N 条）。
    pub async fn loan_history(&self, rows: i64) -> Result<LoanPage> {
        let body = serde_json::json!({
            "page": 1,
            "rows": rows,
            "searchType": 1,
            "searchContent": "",
            "sortType": 0,
            "startDate": serde_json::Value::Null,
            "endDate": serde_json::Value::Null,
        });
        Self::parse_loan_page(self.api_post(LOAN_HISTORY_PATH, body).await?)
    }

    /// 续借一本书。返回平台的提示消息。
    ///
    /// 已知坑：reNew 的确切请求体没有抓包验证过（当时无在借书），这里把记录里
    /// 全部定位字段一起带上；若服务端回「参数异常 {字段名}」，把该字段补进
    /// [`LoanRecord::renew_payload`] 即可。
    pub async fn renew(&self, record: &LoanRecord) -> Result<String> {
        let value = self.api_post(RENEW_PATH, record.renew_payload()).await?;
        if value.get("success") != Some(&serde_json::Value::Bool(true)) {
            bail!(
                "续借被拒：{}",
                value
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
            );
        }
        Ok(value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("续借成功")
            .to_owned())
    }

    /// PC 公开统一检索（find/unify/search）。免登录，groupCode+mappingPath=gzus
    /// 两个头即可；**绝不能带 cookie**（带 oga SESSION 会被当登录态校验 → 401），
    /// 所以走不带 jar 的 http_public 客户端。
    pub async fn search_books(&self, query: &str, page: u32, rows: u32) -> Result<SearchPage> {
        let body = serde_json::json!({
            "searchField": "keyWord",
            "searchFieldContent": query,
            "matchMode": "2",
            "page": page,
            "rows": rows,
            "sortField": "in_date",
            "sortClause": "desc",
        });
        let value = self.find_public_post("/find/unify/search", body).await?;
        let data = value.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let num_found = data
            .get("numFound")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let books = data
            .get("searchResult")
            .and_then(serde_json::Value::as_array)
            .map(|list| list.iter().map(parse_search_book).collect())
            .unwrap_or_default();
        Ok(SearchPage { num_found, books })
    }

    /// 单册馆藏（find/physical/groupitems）：索书号、馆藏地、在架状态。同样免登录。
    pub async fn holdings(&self, record_id: i64) -> Result<Vec<HoldingItem>> {
        let body = serde_json::json!({ "recordId": record_id, "page": 1, "rows": 20 });
        let value = self.find_public_post("/find/physical/groupitems", body).await?;
        Ok(value
            .get("data")
            .and_then(serde_json::Value::as_array)
            .map(|list| list.iter().map(parse_holding).collect())
            .unwrap_or_default())
    }

    /// find 公开 API 调用（只有 groupCode + mappingPath 两个头，不带鉴权不带 cookie）。
    async fn find_public_post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let url = format!("{}{path}", self.find_base);
        let response = self
            .http_public
            .post(&url)
            .header("groupCode", self.group_code.as_str())
            .header("mappingPath", self.find_mapping_path.as_str())
            .json(&body)
            .send()
            .await
            .with_context(|| format!("find 公开检索请求失败: {path}"))?;
        let value: serde_json::Value = response
            .json()
            .await
            .with_context(|| format!("find 公开检索响应不是 JSON: {path}"))?;
        if value.get("success") != Some(&serde_json::Value::Bool(true)) {
            bail!(
                "find 公开检索被拒（{}）：{}",
                value
                    .get("errCode")
                    .and_then(serde_json::Value::as_i64)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into()),
                value
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
            );
        }
        Ok(value)
    }
}

/// 宽容解析一条借阅/历史记录。
fn parse_record(item: &serde_json::Value) -> LoanRecord {
    let get = |key: &str| -> Option<String> {
        item.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    LoanRecord {
        title: get("title"),
        author: get("author"),
        isbn: get("isbn"),
        loan_date: get("loanDate"),
        norm_return_date: get("normReturnDate"),
        return_date: get("returnDate"),
        location_name: get("locationName"),
        barcode: get("barcode"),
        prop_no: get("propNo"),
        loan_id: get("loanId"),
        record_id: get("recordId"),
    }
}

/// 解析一条统一检索结果。
fn parse_search_book(item: &serde_json::Value) -> SearchBook {
    let get = |key: &str| -> Option<String> {
        item.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let call_no = get("callNoOne").or_else(|| {
        item.get("callNo")
            .and_then(serde_json::Value::as_array)
            .and_then(|list| list.first())
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    });
    SearchBook {
        record_id: item
            .get("recordId")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        title: get("title").unwrap_or_default(),
        author: get("author"),
        publisher: get("publisher"),
        publish_year: get("publishYear"),
        isbn: get("isbn"),
        call_no,
        physical_count: item
            .get("physicalCount")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
    }
}

/// 解析一条单册馆藏。
fn parse_holding(item: &serde_json::Value) -> HoldingItem {
    let get = |key: &str| -> Option<String> {
        item.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    HoldingItem {
        call_no: get("callNo"),
        barcode: get("barcode"),
        location_name: get("locationName"),
        cur_location_name: get("curLocationName"),
        process_type: get("processType"),
    }
}

#[cfg(test)]
mod tests {
    use super::{LoanRecord, extract_jwt, extract_sso_params, jwt_ttl, parse_record};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    #[test]
    fn extracts_sso_params_from_html() {
        let html = r#"<html><script>var data = "abcd1234"; var time = "1730000000000";
            var enc = "ABCDEF1234567890ABCDEF1234567890";</script></html>"#;
        let (data, time, enc) = extract_sso_params(html).unwrap();
        assert_eq!(data, "abcd1234");
        assert_eq!(time, "1730000000000");
        assert_eq!(enc, "ABCDEF1234567890ABCDEF1234567890");
        assert!(extract_sso_params("<html>no params</html>").is_none());
    }

    #[test]
    fn extracts_jwt_from_landing_url_fragment() {
        let url = "https://mfindgzus.libsp.cn/#/Home?jwt=abc.def.ghi&jwtHeader=jwtOpacAuth";
        let (jwt, header) = extract_jwt(url).unwrap();
        assert_eq!(jwt, "abc.def.ghi");
        assert_eq!(header, "jwtOpacAuth");
        // jwtHeader 缺省时用当前已知值。
        let (jwt, header) = extract_jwt("https://x.test/#/Home?jwt=zzz").unwrap();
        assert_eq!(jwt, "zzz");
        assert_eq!(header, "jwtOpacAuth");
        assert!(extract_jwt("https://x.test/login").is_none());
    }

    fn make_jwt(exp_offset: i64) -> String {
        let payload = serde_json::json!({ "exp": ru41::util::now() + exp_offset, "sub": "u" });
        format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        )
    }

    #[test]
    fn jwt_ttl_reads_exp() {
        assert!(jwt_ttl(&make_jwt(3600)).is_some_and(|ttl| (3500..3700).contains(&ttl)));
        assert!(jwt_ttl(&make_jwt(-100)).is_some_and(|ttl| ttl < 0));
        assert!(jwt_ttl("not-a-jwt").is_none());
    }

    #[test]
    fn record_key_and_payload() {
        let record = LoanRecord {
            title: Some("Rust 程序设计".into()),
            record_id: Some("r1".into()),
            barcode: Some("b1".into()),
            ..Default::default()
        };
        assert_eq!(record.key(), "r1");
        assert_eq!(record.display_title(), "Rust 程序设计");
        let payload = record.renew_payload();
        assert_eq!(payload["barcode"], "b1");
        // 无任何定位字段时 key 退化为 书名|应还日。
        let bare = LoanRecord::default();
        assert_eq!(bare.key(), "|");
        assert_eq!(bare.display_title(), "条码 ?");
    }

    #[test]
    fn parses_record_from_api_json() {
        let item: serde_json::Value = serde_json::json!({
            "title": "算法导论",
            "author": "Cormen",
            "loanDate": "2026-10-01",
            "normReturnDate": "2026-11-01",
            "locationName": "计算机书库",
            "barcode": "T123",
            "recordId": "rec1"
        });
        let record = parse_record(&item);
        assert_eq!(record.norm_return_date.as_deref(), Some("2026-11-01"));
        assert_eq!(record.location_name.as_deref(), Some("计算机书库"));
        assert!(record.return_date.is_none());
    }
}
