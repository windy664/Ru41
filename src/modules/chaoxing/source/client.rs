//! 学习通 HTTP 协议客户端：登录（AES）、会话 cookie 持久化、
//! 课程/活动/签到/云盘传图/作业列表抓取。

use super::model::{ChaoxingActivity, ChaoxingCourse, ChaoxingWork, CourseEncs, SignMethod};
use super::parse::{capture, decode_course_list, parse_work_deadline, parse_work_list};
use anyhow::{Context, Result, bail};
use base64::Engine;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

const LOGIN_PATH: &str = "/fanyalogin";
const ACCOUNT_PATH: &str = "/mooc/accountManage";
const COURSE_LIST_PATH: &str = "/mooc2-ans/visit/courselistdata";
const ACTIVITY_LIST_PATH: &str = "/v2/apis/active/student/activelist";
const PRE_SIGN_PATH: &str = "/newsign/preSign";
const SIGN_PATH: &str = "/pptSign/stuSignajax";
const PAN_TOKEN_PATH: &str = "/api/token/uservalid";
const PAN_UPLOAD_PATH: &str = "/upload";
const STU_COURSE_MIDDLE_PATH: &str = "/visit/stucoursemiddle";
const WORK_LIST_PATH: &str = "/mooc2/work/list";

const AES_KEY: &[u8; 16] = b"u2oh6Vu^HWe4_AES";
type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

const PASSPORT_BASE: &str = "https://passport2.chaoxing.com";
const MOOC_BASE: &str = "https://mooc2-ans.chaoxing.com";
const MOBILELEARN_BASE: &str = "https://mobilelearn.chaoxing.com";
const PAN_BASE: &str = "https://pan-yz.chaoxing.com";
const MOOC1_BASE: &str = "https://mooc1.chaoxing.com";
const CAPTCHA_BASE: &str = "https://captcha.chaoxing.com";

#[derive(Debug, Deserialize)]
struct LoginResponse {
    #[serde(default)]
    status: Option<bool>,
    #[serde(default)]
    msg2: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityListResponse {
    #[serde(default)]
    result: Option<i64>,
    #[serde(default)]
    error_msg: Option<String>,
    #[serde(default)]
    data: Option<ActivityData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityData {
    #[serde(default)]
    active_list: Vec<ChaoxingActivity>,
}

/// 最小 cookie 存储：仅按 (域名后缀, 名字) 存值，够学习通这些接口用，
/// 可序列化持久化，避免每轮轮询都走账密登录。
#[derive(Default)]
struct CookieJar {
    cookies: Mutex<HashMap<(String, String), String>>,
}

impl CookieJar {
    fn host_matches(domain: &str, host: &str) -> bool {
        host == domain || host.ends_with(&format!(".{domain}"))
    }
}

impl reqwest::cookie::CookieStore for CookieJar {
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

/// cookie 的 JSON 持久化格式。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PersistedCookie {
    domain: String,
    name: String,
    value: String,
}

/// 学习通协议客户端。凭证来自 [sources.chaoxing]（建议环境变量注入）。
#[derive(Clone)]
pub struct ChaoxingClient {
    jar: std::sync::Arc<CookieJar>,
    http: reqwest::Client,
    phone: String,
    password: String,
    cookie_path: PathBuf,
    /// 五个域的基址；测试注入本地 fixture 服务，生产用官方域名。
    passport_base: String,
    mooc_base: String,
    mobilelearn_base: String,
    pan_base: String,
    /// 作业链路域（stucoursemiddle / mooc2/work/list / work 详情）。
    mooc1_base: String,
    /// 滑块验证码域（90002 风控破局，captcha.chaoxing.com）。
    captcha_base: String,
}

impl ChaoxingClient {
    pub fn new(
        phone: impl Into<String>,
        password: impl Into<String>,
        cookie_path: PathBuf,
    ) -> Self {
        Self::with_base_urls(
            phone,
            password,
            cookie_path,
            PASSPORT_BASE.into(),
            MOOC_BASE.into(),
            MOBILELEARN_BASE.into(),
            PAN_BASE.into(),
            MOOC1_BASE.into(),
            CAPTCHA_BASE.into(),
        )
    }

    /// 测试用：六个域基址可指向本地 fixture 服务。
    #[allow(clippy::too_many_arguments)]
    pub fn with_base_urls(
        phone: impl Into<String>,
        password: impl Into<String>,
        cookie_path: PathBuf,
        passport_base: String,
        mooc_base: String,
        mobilelearn_base: String,
        pan_base: String,
        mooc1_base: String,
        captcha_base: String,
    ) -> Self {
        let jar = std::sync::Arc::new(CookieJar::default());
        let http = reqwest::Client::builder()
            .user_agent(concat!("ru41/", env!("CARGO_PKG_VERSION")))
            .timeout(crate::util::HTTP_TIMEOUT)
            .cookie_provider(jar.clone())
            .build()
            .expect("reqwest client build");
        Self {
            jar,
            http,
            phone: phone.into(),
            password: password.into(),
            cookie_path,
            passport_base,
            mooc_base,
            mobilelearn_base,
            pan_base,
            mooc1_base,
            captcha_base,
        }
    }

    /// 从本地恢复 cookie；返回是否恢复出非空会话。
    fn load_cookies(&self) -> bool {
        let Ok(raw) = std::fs::read_to_string(&self.cookie_path) else {
            return false;
        };
        let Ok(list) = serde_json::from_str::<Vec<PersistedCookie>>(&raw) else {
            return false;
        };
        let mut jar = self.jar.cookies.lock().unwrap();
        for c in list {
            jar.insert((c.domain, c.name), c.value);
        }
        !jar.is_empty()
    }

    fn save_cookies(&self) -> Result<()> {
        let jar = self.jar.cookies.lock().unwrap();
        let list: Vec<PersistedCookie> = jar
            .iter()
            .map(|((domain, name), value)| PersistedCookie {
                domain: domain.clone(),
                name: name.clone(),
                value: value.clone(),
            })
            .collect();
        drop(jar);
        if let Some(parent) = self.cookie_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
        let tmp = self.cookie_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string(&list)?)?;
        std::fs::rename(&tmp, &self.cookie_path)?;
        Ok(())
    }

    /// 账密登录；成功后 cookie 持久化。
    pub async fn login(&self) -> Result<()> {
        let response: LoginResponse = self
            .http
            .post(format!("{}{LOGIN_PATH}", self.passport_base))
            .form(&[
                ("fid", "-1"),
                ("uname", &cx_encrypt(&self.phone)),
                ("password", &cx_encrypt(&self.password)),
                ("refer", "https%3A%2F%2Fi.chaoxing.com"),
                ("t", "true"),
                ("forbidotherlogin", "0"),
                ("validate", ""),
                ("doubleFactorLogin", "0"),
                ("independentId", "0"),
            ])
            .send()
            .await
            .context("学习通登录请求失败")?
            .json()
            .await
            .context("学习通登录响应不是合法 JSON")?;
        if response.status != Some(true) {
            bail!(
                "学习通登录被拒绝: {}",
                response.msg2.unwrap_or_else(|| "未知原因".into())
            );
        }
        self.save_cookies()
    }

    /// 会话是否有效（accountManage 页面能取到姓名）。
    async fn session_valid(&self) -> bool {
        self.realname().await.is_some_and(|name| !name.is_empty())
    }

    /// 当前登录用户姓名；会话无效时返回 None。
    pub async fn realname(&self) -> Option<String> {
        let text = self
            .http
            .get(format!("{}{ACCOUNT_PATH}", self.passport_base))
            .send()
            .await
            .ok()?
            .text()
            .await
            .ok()?;
        let re = regex::Regex::new(r#"id="messageName"\s+value="([^"]*)""#).ok()?;
        Some(re.captures(&text)?.get(1)?.as_str().to_owned())
    }

    /// 确保会话可用：本地 cookie 有效则复用，否则账密登录（每进程最多一次登录）。
    pub async fn ensure_session(&self) -> Result<()> {
        if self.load_cookies() && self.session_valid().await {
            return Ok(());
        }
        self.login().await?;
        if !self.session_valid().await {
            bail!("学习通登录后仍取不到会话，可能触发风控，请稍后再试");
        }
        Ok(())
    }

    /// 取 cookie 值（fid / _uid 之类）。
    fn cookie(&self, name: &str) -> Option<String> {
        let jar = self.jar.cookies.lock().unwrap();
        jar.iter()
            .find(|((_, key), value)| key == name && !value.is_empty())
            .map(|(_, value)| value.clone())
    }

    pub fn uid(&self) -> Result<String> {
        self.cookie("_uid")
            .or_else(|| self.cookie("UID"))
            .context("会话 cookie 中没有 _uid，请重新登录")
    }

    fn fid(&self) -> String {
        self.cookie("fid").unwrap_or_else(|| "1024".into())
    }

    /// 课程列表（courselistdata HTML → div.course 块解析；未开放课程跳过）。
    pub async fn course_list(&self) -> Result<Vec<ChaoxingCourse>> {
        let html = self
            .http
            .post(format!("{}{COURSE_LIST_PATH}", self.mooc_base))
            .header(
                "Referer",
                "https://mooc2-ans.chaoxing.com/mooc2-ans/visit/interaction",
            )
            .form(&[
                ("courseType", "1"),
                ("courseFolderId", "0"),
                ("query", ""),
                ("superstarClass", "0"),
            ])
            .send()
            .await
            .context("课程列表请求失败")?
            .text()
            .await
            .context("课程列表响应读取失败")?;
        Ok(decode_course_list(&html))
    }

    /// 某门课程的活动列表（含非签到活动，调用方按 is_sign 过滤）。
    pub async fn activity_list(&self, course: &ChaoxingCourse) -> Result<Vec<ChaoxingActivity>> {
        let response: ActivityListResponse = self
            .http
            .get(format!("{}{ACTIVITY_LIST_PATH}", self.mobilelearn_base))
            .query(&[
                ("fid", self.fid()),
                ("courseId", course.course_id.clone()),
                ("classId", course.clazz_id.clone()),
                ("showNotStartedActive", "0".to_string()),
                ("_", crate::util::now().to_string()),
            ])
            .send()
            .await
            .context("活动列表请求失败")?
            .json()
            .await
            .context("活动列表响应不是合法 JSON（可能登录态失效）")?;
        if response.result != Some(1) {
            bail!(
                "活动列表返回异常: {}",
                response
                    .error_msg
                    .unwrap_or_else(|| format!("result={:?}", response.result))
            );
        }
        Ok(response.data.map(|d| d.active_list).unwrap_or_default())
    }

    /// 手动签到（全类型）。统一端点 stuSignajax，差异仅在参数填充；
    /// 参数形状对齐社区长期在用的实现（preSign 仅普通类需要）。
    /// 被风控拦下（响应 90002 或 validate*）时自动过滑块拿 validate 重签。
    pub async fn sign(
        &self,
        course: &ChaoxingCourse,
        activity: &ChaoxingActivity,
        method: &SignMethod,
    ) -> Result<String> {
        let activity_id = activity.id.to_string();
        let uid = self.uid()?;
        // 拍照/位置/二维码的教师端展示名用真实姓名，普通类传空即可。
        let display_name = match method {
            SignMethod::Normal => String::new(),
            _ => self.realname().await.unwrap_or_default(),
        };

        if matches!(method, SignMethod::Normal) {
            // preSign 是平台流程的一部分，缺失会被部分课程判为异常签到
            self.pre_sign(course, &activity_id, &uid).await?;
        }

        let mut params: Vec<(String, String)> = vec![
            ("activeId".into(), activity_id.clone()),
            ("uid".into(), uid.clone()),
            ("fid".into(), self.fid()),
            ("clientip".into(), String::new()),
            ("useragent".into(), String::new()),
            ("appType".into(), "15".into()),
        ];
        match method {
            SignMethod::Normal => {
                params.push(("objectId".into(), "aaa".into()));
                params.push(("name".into(), String::new()));
                params.push(("latitude".into(), "-1".into()));
                params.push(("longitude".into(), "-1".into()));
            }
            SignMethod::Location { lat, lon, addr } => {
                params.push(("name".into(), display_name));
                params.push(("address".into(), addr.clone()));
                params.push(("latitude".into(), lat.to_string()));
                params.push(("longitude".into(), lon.to_string()));
                params.push(("ifTiJiao".into(), "1".into()));
            }
            SignMethod::Qr { enc, location } => {
                let (lat, lon, addr) = location
                    .as_ref()
                    .map(|(lat, lon, addr)| (lat.to_string(), lon.to_string(), addr.clone()))
                    .unwrap_or_else(|| ("0".into(), "0".into(), String::new()));
                let blob = format!(
                    r#"{{"result":"1","address":"{addr}","latitude":{lat},"longitude":{lon},"altitude":0}}"#
                );
                params.push(("enc".into(), enc.clone()));
                params.push(("name".into(), display_name));
                params.push(("location".into(), blob));
                params.push(("latitude".into(), "-1".into()));
                params.push(("longitude".into(), "-1".into()));
            }
            SignMethod::Photo { path } => {
                let object_id = self.upload_photo(path).await?;
                params.push(("objectId".into(), object_id));
                params.push(("name".into(), display_name));
                params.push(("latitude".into(), "-1".into()));
                params.push(("longitude".into(), "-1".into()));
            }
        }

        let text = self.send_sign(&params).await?;
        if !sign_needs_captcha(&text) {
            return Ok(text);
        }
        // 风控拦截（2026-10-30 实测手势直签稳定 90002；社区记录 validate_* 形态）：
        // validate_ 前缀会带回 enc2，重签时一并捎上；滑块最多换图试两次，
        // 仍失败则把原始拒绝响应交回上层（手势会落到 🚨 手动签提醒）。
        if let Some(enc2) = text
            .strip_prefix("validate_")
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
        {
            params.push(("enc2".into(), enc2));
        }
        let referer = self.pre_sign_url(course, &activity_id, &uid);
        for _ in 0..2 {
            let Ok(validate) = self.solve_slider_captcha(&referer).await else {
                break;
            };
            let mut retry = params.clone();
            retry.push(("validate".into(), validate));
            let retry_text = self.send_sign(&retry).await?;
            if !sign_needs_captcha(&retry_text) {
                return Ok(retry_text);
            }
        }
        Ok(text)
    }

    /// stuSignajax 请求发送（签到主路径与滑块重签共用）。
    async fn send_sign(&self, params: &[(String, String)]) -> Result<String> {
        self.http
            .get(format!("{}{SIGN_PATH}", self.mobilelearn_base))
            .query(params)
            .send()
            .await
            .context("签到请求失败")?
            .text()
            .await
            .context("签到响应读取失败")
    }

    /// 滑块验证全流程：conf 拿服务端时间戳 → 本地 md5 构造 image 参数 →
    /// 拉底图/拼块图 → 模板匹配识别缺口 x → check 拿 validate。
    /// 方案对齐社区 ChaoxingSignFaker 的纯 HTTP 实现（无 WebView 依赖）。
    pub async fn solve_slider_captcha(&self, referer: &str) -> Result<String> {
        use super::captcha;
        let now = crate::util::now();
        let conf = self
            .http
            .get(format!("{}/captcha/get/conf", self.captcha_base))
            .query(&[
                ("callback", "cx_captcha_function"),
                ("captchaId", captcha::CAPTCHA_ID),
                ("_", &now.to_string()),
            ])
            .send()
            .await
            .context("captcha conf 请求失败")?
            .text()
            .await
            .context("captcha conf 响应读取失败")?;
        let t = captcha::parse_conf_t(&conf)?;

        let (captcha_key, iv, image_token) = captcha::image_params(t, crate::util::now());
        let image = self
            .http
            .get(format!(
                "{}/captcha/get/verification/image",
                self.captcha_base
            ))
            .query(&[
                ("callback", "cx_captcha_function"),
                ("captchaId", captcha::CAPTCHA_ID),
                ("type", "slide"),
                ("version", captcha::CAPTCHA_VERSION),
                ("captchaKey", captcha_key.as_str()),
                ("token", image_token.as_str()),
                ("referer", referer),
                ("iv", iv.as_str()),
                ("_", &crate::util::now().to_string()),
            ])
            .send()
            .await
            .context("captcha image 请求失败")?
            .text()
            .await
            .context("captcha image 响应读取失败")?;
        let (server_token, shade_url, cutout_url) = captcha::parse_image_response(&image)?;

        let shade = self
            .http
            .get(&shade_url)
            .send()
            .await
            .context("滑块底图下载失败")?
            .bytes()
            .await
            .context("滑块底图读取失败")?;
        let cutout = self
            .http
            .get(&cutout_url)
            .send()
            .await
            .context("滑块拼块图下载失败")?
            .bytes()
            .await
            .context("滑块拼块图读取失败")?;
        let x = captcha::slider_gap_x(&shade, &cutout).context("滑块缺口识别失败")?;

        let click_arr = format!("[{{\"x\":{x}}}]");
        let check = self
            .http
            .get(format!(
                "{}/captcha/check/verification/result",
                self.captcha_base
            ))
            .query(&[
                ("callback", "cx_captcha_function"),
                ("captchaId", captcha::CAPTCHA_ID),
                ("type", "slide"),
                ("token", server_token.as_str()),
                ("textClickArr", click_arr.as_str()),
                ("coordinate", "[]"),
                ("runEnv", "10"),
                ("version", captcha::CAPTCHA_VERSION),
                ("t", "a"),
                ("iv", iv.as_str()),
                ("_", &crate::util::now().to_string()),
            ])
            .header("Referer", referer)
            .send()
            .await
            .context("captcha check 请求失败")?
            .text()
            .await
            .context("captcha check 响应读取失败")?;
        captcha::parse_check_validate(&check)?.context("滑块位置被拒绝")
    }

    /// preSign 预签（普通类签到的平台流程前置）。
    async fn pre_sign(&self, course: &ChaoxingCourse, activity_id: &str, uid: &str) -> Result<()> {
        self.pre_sign_body(course, activity_id, uid)
            .await
            .map(|_| ())
    }

    /// preSign 完整 URL（滑块 check 的 Referer 用，平台按它核对来源场景）。
    fn pre_sign_url(&self, course: &ChaoxingCourse, activity_id: &str, uid: &str) -> String {
        format!(
            "{}{PRE_SIGN_PATH}?general=1&sys=1&ls=1&appType=15&tid=&ut=s&uid={}&activePrimaryId={}&courseId={}&classId={}",
            self.mobilelearn_base, uid, activity_id, course.course_id, course.clazz_id
        )
    }

    /// preSign 响应体（科研线：窗口探测用它找 enc 泄漏）。
    pub async fn pre_sign_body(
        &self,
        course: &ChaoxingCourse,
        activity_id: &str,
        uid: &str,
    ) -> Result<String> {
        self.http
            .get(format!("{}{PRE_SIGN_PATH}", self.mobilelearn_base))
            .query(&[
                ("general", "1".to_string()),
                ("sys", "1".to_string()),
                ("ls", "1".to_string()),
                ("appType", "15".to_string()),
                ("tid", String::new()),
                ("ut", "s".to_string()),
                ("uid", uid.to_string()),
                ("activePrimaryId", activity_id.to_string()),
                ("courseId", course.course_id.clone()),
                ("classId", course.clazz_id.clone()),
            ])
            .send()
            .await
            .context("preSign 请求失败")?
            .text()
            .await
            .context("preSign 响应读取失败")
    }

    /// 学生端签到详情页 HTML（科研线：QR 窗口期间侦测 enc 是否泄漏在页面里）。
    pub async fn sign_page_html(&self, activity_id: i64) -> Result<String> {
        let uid = self.uid()?;
        let url = format!(
            "{}/widget/sign/pcStuSignController?activeId={}&uid={}&fid={}",
            self.mobilelearn_base,
            activity_id,
            uid,
            self.fid()
        );
        self.http
            .get(&url)
            .send()
            .await
            .context("签到详情页请求失败")?
            .error_for_status()
            .context("签到详情页响应异常")?
            .text()
            .await
            .context("签到详情页读取失败")
    }

    /// 上传照片到超星云盘换 objectId（拍照签到前置）。
    /// 流程：uservalid 取 _token → multipart 上传（file + puid）→ 返回 objectId。
    async fn upload_photo(&self, path: &std::path::Path) -> Result<String> {
        let bytes =
            std::fs::read(path).with_context(|| format!("读取照片失败: {}", path.display()))?;
        let token: serde_json::Value = self
            .http
            .get(format!("{}{PAN_TOKEN_PATH}", self.pan_base))
            .send()
            .await
            .context("云盘 token 请求失败")?
            .json()
            .await
            .context("云盘 token 响应不是合法 JSON")?;
        let token = token
            .get("_token")
            .and_then(|v| v.as_str())
            .context("云盘 token 获取失败（登录态可能失效）")?
            .to_owned();
        let uid = self.uid()?;
        let boundary = format!("----ru41{}", crate::util::now());
        let mut body = Vec::with_capacity(bytes.len() + 512);
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"0.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(&bytes);
        body.extend_from_slice(
            format!(
                "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"puid\"\r\n\r\n{uid}\r\n--{boundary}--\r\n"
            )
            .as_bytes(),
        );
        let resp: serde_json::Value = self
            .http
            .post(format!(
                "{}{PAN_UPLOAD_PATH}?_from=mobilelearn&_token={token}",
                self.pan_base
            ))
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .context("照片上传请求失败")?
            .json()
            .await
            .context("照片上传响应不是合法 JSON")?;
        resp.get("objectId")
            .or_else(|| resp.get("data").and_then(|d| d.get("objectId")))
            .and_then(|v| v.as_str().map(str::to_owned))
            .context("照片上传响应缺少 objectId")
    }

    /// 进课取作业鉴权三件套（stucoursemiddle 隐藏域 enc/openc/workEnc）。
    pub async fn course_encs(&self, course: &ChaoxingCourse) -> Result<CourseEncs> {
        let html = self
            .http
            .get(format!(
                "{}{STU_COURSE_MIDDLE_PATH}?courseid={}&clazzid={}&cpi={}&ismooc2=1&v=2",
                self.mooc1_base, course.course_id, course.clazz_id, course.cpi
            ))
            .send()
            .await
            .context("课程中间页请求失败")?
            .text()
            .await
            .context("课程中间页响应读取失败")?;
        // 属性顺序容忍：id 与 value 之间可能夹 name 等属性
        let grab = |name: &str| {
            capture(&html, &format!(r#"id="{name}"[^>]*value="([^"]*)""#))
                .with_context(|| format!("课程中间页缺少 {name}（页面可能改版）"))
        };
        Ok(CourseEncs {
            enc: grab("enc")?,
            openc: grab("openc")?,
            work_enc: grab("workEnc")?,
        })
    }

    /// 作业列表（mooc2/work/list，enc 必须用 workEnc，t/stuenc 伴随）。
    /// 分页按页面 `pageNum : N` 提示翻页（上限 10 页，防异常数据打爆请求）。
    pub async fn work_list(
        &self,
        course: &ChaoxingCourse,
        encs: &CourseEncs,
    ) -> Result<Vec<ChaoxingWork>> {
        let fetch = |page: u32| {
            let url = format!(
                "{}{WORK_LIST_PATH}?courseId={}&classId={}&cpi={}&ut=s&openc={}&enc={}&t={}&stuenc={}&isdisplaytable=2&pageNum={page}",
                self.mooc1_base,
                course.course_id,
                course.clazz_id,
                course.cpi,
                encs.openc,
                encs.work_enc,
                crate::util::now(),
                encs.enc
            );
            async move {
                self.http
                    .get(&url)
                    .send()
                    .await
                    .context("作业列表请求失败")?
                    .text()
                    .await
                    .context("作业列表响应读取失败")
            }
        };
        let first = fetch(1).await?;
        if first.contains("无权限") || first.contains("重新进入") {
            bail!("作业列表无权限（workEnc 失效或课程会话过期）");
        }
        let pages = capture(&first, r"pageNum\s*:\s*(\d+)")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1)
            .clamp(1, 10);
        let mut works = parse_work_list(&first);
        for page in 2..=pages {
            let html = fetch(page).await?;
            works.extend(parse_work_list(&html));
        }
        Ok(works)
    }

    /// 详情页拉截止时间。平台渲染「作答时间: … 至 …」，结束时刻即截止。
    /// 只对未交作业调用，控制请求量。
    pub async fn work_deadline(&self, detail_url: &str) -> Result<Option<String>> {
        let html = self
            .http
            .get(detail_url)
            .send()
            .await
            .context("作业详情请求失败")?
            .text()
            .await
            .context("作业详情响应读取失败")?;
        Ok(parse_work_deadline(&html, crate::util::now()))
    }
}

fn cx_encrypt(plaintext: &str) -> String {
    let encrypted =
        Aes128CbcEnc::new(AES_KEY.into(), AES_KEY.into())
            .encrypt_padded_vec_mut::<cbc::cipher::block_padding::Pkcs7>(plaintext.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(encrypted)
}

/// 签到响应是否命中风控、需要先过滑块：90002（2026-10-30 手势实测）
/// 或 validate_*（社区记录的另一形态，validate_ 前缀会带回 enc2）。
fn sign_needs_captcha(response: &str) -> bool {
    response.starts_with("validate") || response.contains("90002")
}

/// 解析 courselistdata 返回的课程列表 HTML。
/// 按协议库（BeautifulSoup 选择器）语义做分块正则提取；页面改版会集中体现在此函数。
/// 真实页面的课程块形如 class="course clearfix learnCourse stu_157361070"，
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_matches_reference_vectors() {
        assert_eq!(cx_encrypt("10000000000"), "oc39XnXWPbS2hxKxU/rZhw==");
        assert_eq!(cx_encrypt("test-password"), "xZ9dYlVASzFVINL+G69JXA==");
    }

    #[test]
    fn cookie_jar_domain_and_persist_shape() {
        use reqwest::cookie::CookieStore;
        let jar = CookieJar::default();
        let url: url::Url = "https://passport2.chaoxing.com/fanyalogin".parse().unwrap();
        let set = reqwest::header::HeaderValue::from_str(
            "_uid=487480814; Path=/; Domain=.chaoxing.com; HttpOnly",
        )
        .unwrap();
        let it = [set];
        jar.set_cookies(&mut it.iter(), &url);
        let other: url::Url = "https://mobilelearn.chaoxing.com/v2/apis/x"
            .parse()
            .unwrap();
        let header = jar.cookies(&other).unwrap();
        assert!(header.to_str().unwrap().contains("_uid=487480814"));
    }

    #[test]
    fn activity_list_response_parses_camel_case_error_msg() {
        let raw = r#"{"result":0,"msg":null,"data":null,"errorMsg":"请勿频繁操作"}"#;
        let response: ActivityListResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(response.result, Some(0));
        assert_eq!(response.error_msg.as_deref(), Some("请勿频繁操作"));
        let raw = r#"{"result":1,"data":{"activeList":[]}}"#;
        let response: ActivityListResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(response.result, Some(1));
        assert!(response.error_msg.is_none());
    }
}
