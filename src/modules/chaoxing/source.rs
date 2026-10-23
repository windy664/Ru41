//! 学习通（超星）协议客户端：登录、课程列表、签到活动、手动签到。
//!
//! 协议为纯 HTTP（无浏览器、无验证码路径），端点经社区多项目长期验证：
//! 登录 `fanyalogin`（AES-CBC 加密账密）、课程列表 `courselistdata`（HTML），
//! 活动列表与签到在 mobilelearn 域（JSON / GET 参数）。会话 cookie 持久化到
//! 本地文件，轮询时校验复用，失效才重登录（降频保号）。

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

const PASSPORT_BASE: &str = "https://passport2.chaoxing.com";
const MOOC_BASE: &str = "https://mooc2-ans.chaoxing.com";
const MOBILELEARN_BASE: &str = "https://mobilelearn.chaoxing.com";
const PAN_BASE: &str = "https://pan-yz.chaoxing.com";
const MOOC1_BASE: &str = "https://mooc1.chaoxing.com";

/// 内置兜底签到照片（与远端 cxsign 曾用图为同一张，平台已验证可用）。
/// 仅在未配置 `photo_path` 且命令行未给 `--photo` 时使用。
pub fn fallback_photo_bytes() -> &'static [u8] {
    include_bytes!("assets/black.jpg")
}

/// 签到方式：统一走 stuSignajax，类型差异只体现在参数填充上。
#[derive(Debug, Clone, PartialEq)]
pub enum SignMethod {
    /// 普通 / 手势 / 签到码（平台不校验额外参数）。
    Normal,
    /// 位置签到：经纬度 + 地址文本（address 参数，教师端可见）。
    Location { lat: f64, lon: f64, addr: String },
    /// 二维码签到：enc 为现场二维码内容里的时效 token，过期需重新扫。
    /// location 为可选的坐标/地址（随 location JSON 参数上报）。
    Qr {
        enc: String,
        location: Option<(f64, f64, String)>,
    },
    /// 拍照签到：path 为本地照片文件，先传云盘换 objectId 再签。
    Photo { path: PathBuf },
}

/// 课程上下文令牌（stucoursemiddle 服务端渲染的隐藏域），作业列表鉴权三件套。
#[derive(Debug, Clone, PartialEq)]
pub struct CourseEncs {
    /// 页面通用 enc（stuenc 伴随参数）。
    pub enc: String,
    pub openc: String,
    /// 作业 tab 专用 enc——必须用它而非页面 enc，否则「无权限」。
    pub work_enc: String,
}

/// 作业条目（mooc2/work/list 的 li[data] 项）。
#[derive(Debug, Clone, PartialEq)]
pub struct ChaoxingWork {
    pub work_id: String,
    pub title: String,
    /// 平台原话状态：未交/已提交/待批阅/待互评/已完成/已过期。
    pub status: String,
    /// 详情页直链（带独立 enc）；拉截止时间用。
    pub detail_url: String,
}

impl ChaoxingWork {
    /// 是否仍未交（状态含「未」：未交/未提交）。
    pub fn is_open(&self) -> bool {
        self.status.contains('未')
    }
}

/// 登录接口的 AES-128-CBC key/iv（社区通用公开值，非机密）。
const AES_KEY: &[u8; 16] = b"u2oh6Vu^HWe4_AES";

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

/// 学习通课程（courselistdata HTML 中 div.course 块）。
#[derive(Debug, Clone, PartialEq)]
pub struct ChaoxingCourse {
    pub course_id: String,
    pub clazz_id: String,
    pub cpi: String,
    pub title: String,
    pub teacher: String,
}

/// 伪造二维码签到 enc：平台对 enc 不做真实性校验（任何 32 位十六进制都收），
/// 用 uid+activityId 派生一个稳定的假 token，形状对齐真实 enc（32 位大写十六进制）。
pub fn fabricated_enc(activity_id: i64, uid: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(uid.as_bytes());
    hasher.update(b":");
    hasher.update(activity_id.to_string().as_bytes());
    let digest = hasher.finalize();
    digest
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join("")
}

/// 签到活动（activelist JSON 条目，camelCase）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChaoxingActivity {
    pub id: i64,
    /// 2 = 签到类活动。
    #[serde(default)]
    pub active_type: Option<i64>,
    /// 签到子类型：0普通 1拍照 2二维码 3手势 4位置 5签到码。
    #[serde(rename = "type", default)]
    pub sign_type: Option<i64>,
    /// 平台类型标签，如「二维码签到」。
    #[serde(default)]
    pub name_one: Option<String>,
    #[serde(default)]
    pub start_time: Option<i64>,
    #[serde(default)]
    pub end_time: Option<i64>,
    /// 1 = 进行中，2 = 已结束。
    #[serde(default)]
    pub status: Option<i64>,
    /// 1 = 未签到。
    #[serde(default)]
    pub user_status: Option<i64>,
    #[serde(default)]
    pub attend_num: Option<i64>,
}

impl ChaoxingActivity {
    pub fn is_sign(&self) -> bool {
        self.active_type == Some(2)
    }
    /// 进行中且本人未签，需要提醒/动手。
    pub fn is_pending(&self) -> bool {
        self.is_sign() && self.status == Some(1) && self.user_status == Some(1)
    }
    /// 签到种类。以平台标签 nameOne 文案为准——实测 activelist 的 type 字段
    /// 恒为 2（与 activeType 同义，指「签到类活动」），并不区分子类型；
    /// nameOne 缺失时才退回 type 字段的文档映射。
    pub fn sign_kind(&self) -> ChaoxingSignKind {
        let label = self.name_one.as_deref().unwrap_or_default();
        if label.contains("二维码") {
            return ChaoxingSignKind::Qr;
        }
        if label.contains("手势") {
            return ChaoxingSignKind::Gesture;
        }
        if label.contains("位置") {
            return ChaoxingSignKind::Location;
        }
        if label.contains("拍照") {
            return ChaoxingSignKind::Photo;
        }
        if label.contains("签到码") {
            return ChaoxingSignKind::Code;
        }
        match self.sign_type {
            Some(1) => ChaoxingSignKind::Photo,
            Some(2) => ChaoxingSignKind::Qr,
            Some(3) => ChaoxingSignKind::Gesture,
            Some(4) => ChaoxingSignKind::Location,
            Some(5) => ChaoxingSignKind::Code,
            _ => ChaoxingSignKind::Normal,
        }
    }
    /// 二维码是唯一不能远程自动签的：enc 是现场二维码内容里的时效 token。
    pub fn is_qr(&self) -> bool {
        self.sign_kind() == ChaoxingSignKind::Qr
    }
    pub fn type_label(&self) -> String {
        self.name_one
            .clone()
            .unwrap_or_else(|| super::service::sign_type_name(self.sign_type).to_owned())
    }
}

/// 签到子类型（识别自 nameOne 文案，见 `sign_kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaoxingSignKind {
    Normal,
    Gesture,
    Location,
    Photo,
    Qr,
    Code,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    #[serde(default)]
    status: Option<bool>,
    #[serde(default)]
    msg2: Option<String>,
}

#[derive(Debug, Deserialize)]
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
        )
    }

    /// 测试用：五个域基址可指向本地 fixture 服务。
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

    fn uid(&self) -> Result<String> {
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
            ("activeId".into(), activity_id),
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

        let text = self
            .http
            .get(format!("{}{SIGN_PATH}", self.mobilelearn_base))
            .query(&params)
            .send()
            .await
            .context("签到请求失败")?
            .text()
            .await
            .context("签到响应读取失败")?;
        Ok(text)
    }

    /// preSign 预签（普通类签到的平台流程前置）。
    async fn pre_sign(&self, course: &ChaoxingCourse, activity_id: &str, uid: &str) -> Result<()> {
        let _: String = self
            .http
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
            .context("preSign 响应读取失败")?;
        Ok(())
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

/// 解析作业列表页：li[data=详情直链] + 标题 + 状态。
fn parse_work_list(html: &str) -> Vec<ChaoxingWork> {
    let re_li =
        regex::Regex::new(r#"(?s)<li[^>]*data="([^"]+)"[^>]*>(.*?)</li>"#).expect("li regex");
    let re_title = regex::Regex::new(r#"class="overHidden2[^"]*">([^<]+)<"#).expect("title regex");
    let re_status = regex::Regex::new(r#"class="status[^"]*">([^<]+)<"#).expect("status regex");
    let re_id = regex::Regex::new(r"[?&]workId=(\d+)").expect("workId regex");
    let mut out = Vec::new();
    for cap in re_li.captures_iter(html) {
        let (url, body) = (&cap[1], &cap[2]);
        let Some(work_id) = re_id.captures(url).map(|c| c[1].to_owned()) else {
            continue;
        };
        out.push(ChaoxingWork {
            work_id,
            title: re_title
                .captures(body)
                .map(|c| c[1].trim().to_owned())
                .unwrap_or_else(|| "未命名作业".into()),
            status: re_status
                .captures(body)
                .map(|c| c[1].trim().to_owned())
                .unwrap_or_else(|| "未知".into()),
            detail_url: url.replace("&amp;", "&"),
        });
    }
    out
}

/// 从详情页提取截止时间，归一化为 "YYYY-MM-DD HH:MM"。
/// 平台渲染「作答时间: MM-DD HH:MM 至 MM-DD HH:MM」（缺年份）；
/// 跨年按「距今不超过 180 天的最近未来」补年。
fn parse_work_deadline(html: &str, now: i64) -> Option<String> {
    // 全格式（含年份）
    if let Some(end) = capture(
        html,
        r"作答时间[^<]*?\d{4}-\d{2}-\d{2} \d{2}:\d{2}[^<]*?至[^<]*?(\d{4}-\d{2}-\d{2} \d{2}:\d{2})",
    ) {
        return Some(end);
    }
    // 缺年份变体：按「距今不超过 180 天的最近合理年份」补全
    let end = capture(
        html,
        r"作答时间[^<]*?\d{2}-\d{2} \d{2}:\d{2}[^<]*?至[^<]*?(\d{2}-\d{2} \d{2}:\d{2})",
    )?;
    let year = infer_deadline_year(now, &end)?;
    Some(format!("{year}-{end}"))
}

/// 把 "MM-DD HH:MM" 补全到距今 180 天内的最近年份。
fn infer_deadline_year(now: i64, end_mmdd: &str) -> Option<i32> {
    use chrono::Datelike;
    let today = chrono::DateTime::from_timestamp(now, 0)?.date_naive();
    let current = today.year();
    for year in [current, current + 1, current - 1] {
        // end_mmdd 形如 "MM-DD HH:MM"，只取前 5 位日期
        let Ok(date) =
            chrono::NaiveDate::parse_from_str(&format!("{year}-{}", &end_mmdd[..5]), "%Y-%m-%d")
        else {
            continue;
        };
        if (-180..=180).contains(&(date - today).num_days()) {
            return Some(year);
        }
    }
    None
}

/// 学习通登录参数加密：AES-128-CBC（key=iv）+ PKCS7 + base64。
fn cx_encrypt(plaintext: &str) -> String {
    let encrypted =
        Aes128CbcEnc::new(AES_KEY.into(), AES_KEY.into())
            .encrypt_padded_vec_mut::<cbc::cipher::block_padding::Pkcs7>(plaintext.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(encrypted)
}

/// 解析 courselistdata 返回的课程列表 HTML。
/// 按协议库（BeautifulSoup 选择器）语义做分块正则提取；页面改版会集中体现在此函数。
/// 真实页面的课程块形如 class="course clearfix learnCourse stu_157361070"，
/// 因此按「class 列表以 course 开头」分块（不会误切 course-cover 等子元素）。
fn decode_course_list(html: &str) -> Vec<ChaoxingCourse> {
    let re_div = regex::Regex::new(r#"<div class="course(?: [^"]*)?""#).expect("course div regex");
    let mut courses = Vec::new();
    for chunk in re_div.split(html).skip(1) {
        if chunk.contains("not-open-tip") {
            continue;
        }
        let Some(course_id) = capture(chunk, r#"name="courseId"\s+value="(\d+)""#) else {
            continue;
        };
        let Some(clazz_id) = capture(chunk, r#"name="clazzId"\s+value="(\d+)""#) else {
            continue;
        };
        let Some(cpi) = capture(chunk, r#"cpi=(\d+)&"#) else {
            continue;
        };
        let Some(title) = capture(chunk, r#"class="course-name[^"]*"[^>]*title="([^"]+)""#) else {
            continue;
        };
        let teacher =
            capture(chunk, r#"class="[^"]*color3[^"]*"[^>]*title="([^"]*)""#).unwrap_or_default();
        courses.push(ChaoxingCourse {
            course_id,
            clazz_id,
            cpi,
            title,
            teacher,
        });
    }
    courses
}

fn capture(text: &str, pattern: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).ok()?;
    re.captures(text)?.get(1).map(|m| m.as_str().to_owned())
}

/// 只在测试中做 AES 向量比对，保证与参考实现字节级一致。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_matches_reference_vectors() {
        assert_eq!(cx_encrypt("15322883180"), "M6ccj1g5qPxp7tsBQ44/6g==");
        assert_eq!(cx_encrypt("test-password"), "xZ9dYlVASzFVINL+G69JXA==");
    }

    #[test]
    fn decode_course_list_skips_not_open() {
        let html = r#"<div class="course" id="c1">
            <input type="hidden" name="clazzId" value="111" class="clazzId">
            <input type="hidden" name="courseId" value="222" class="courseId">
            <a href="https://x.chaoxing.com/learn?cpi=333&x=1"></a>
            <span class="course-name" title="高等数学I(理)">高等数学I(理)</span>
            <p class="color3" title="张老师">张老师</p>
        </div>
        <div class="course" id="c2">
            <a class="not-open-tip"></a>
            <input type="hidden" name="clazzId" value="444" class="clazzId">
            <input type="hidden" name="courseId" value="555" class="courseId">
            <a href="https://x.chaoxing.com/learn?cpi=666&x=1"></a>
            <span class="course-name" title="未开放课">未开放课</span>
        </div>"#;
        let courses = decode_course_list(html);
        assert_eq!(courses.len(), 1);
        assert_eq!(courses[0].course_id, "222");
        assert_eq!(courses[0].clazz_id, "111");
        assert_eq!(courses[0].cpi, "333");
        assert_eq!(courses[0].title, "高等数学I(理)");
        assert_eq!(courses[0].teacher, "张老师");
    }

    #[test]
    fn parse_activity_json() {
        let raw = r#"{"userStatus":1,"nameOne":"二维码签到","attendNum":57,
            "activeType":2,"type":2,"startTime":1792135761000,"id":1000174332257,
            "endTime":1792135881000,"status":2,"unknown_field":true}"#;
        let a: ChaoxingActivity = serde_json::from_str(raw).unwrap();
        assert_eq!(a.id, 1000174332257);
        assert!(a.is_sign());
        assert!(!a.is_pending());
        assert_eq!(a.type_label(), "二维码签到");
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
}
