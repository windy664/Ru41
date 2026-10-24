//! 微助教 HTTP 协议客户端：openid 鉴权（Header）、待签列表、签到、学籍探测。
//! 协议调研与证据链见 docs/teachermate-recon.md。

use super::model::{
    ActiveSign, CourseHomework, HomeworkCourse, SignsOutcome, StudentRole, parse_course_homework,
    parse_homework_courses,
};
use anyhow::{Context, Result, bail};
use reqwest::StatusCode;

const ACTIVE_SIGNS_PATH: &str = "/wechat-api/v1/class-attendance/student/active_signs";
const SIGN_IN_PATH: &str = "/wechat-api/v1/class-attendance/student-sign-in";
const ROLE_PATH: &str = "/wechat-api/v2/students/role";
const HOMEWORK_COURSES_PATH: &str = "/wechat-api/v1/homework/courses";
const HOMEWORK_LIST_PATH: &str = "/wechat-api/v1/homework";

pub const DEFAULT_API_BASE: &str = "https://v18.teachermate.cn";

/// openid 失效的平台特征串（{"message":"登录信息失效，请退出后重试"}）。
/// 2026-10-24 实测：缺失/伪造 openid 均为 HTTP 401 + 该响应体。
pub const EXPIRED_MARKER: &str = "登录信息失效";

/// 失效判定双保险：HTTP 401（实测值）或响应体特征串（防平台改为 200 包裹）。
fn is_expired(status: StatusCode, text: &str) -> bool {
    status == StatusCode::UNAUTHORIZED || text.contains(EXPIRED_MARKER)
}

/// 微助教协议客户端。凭证是人工投喂的 openid（Header 传递，非 cookie）。
#[derive(Clone)]
pub struct TeachermateClient {
    openid: String,
    /// openid 文件（可选）：存在且非空时**每请求热加载**，优先于构造时字符串。
    /// 换绑入口（QQ 粘贴链接 / bind 命令）只写文件，无需重启容器。
    openid_file: Option<std::path::PathBuf>,
    http: reqwest::Client,
    /// 学生端 API 基址；测试注入本地 fixture 服务，生产用官方域名。
    api_base: String,
}

impl TeachermateClient {
    pub fn new(openid: impl Into<String>) -> Self {
        Self::with_api_base(openid, DEFAULT_API_BASE.into())
    }

    /// 测试用：基址可指向本地 fixture 服务。
    pub fn with_api_base(openid: impl Into<String>, api_base: String) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ru41/", env!("CARGO_PKG_VERSION")))
            .timeout(crate::util::HTTP_TIMEOUT)
            .build()
            .expect("reqwest client build");
        Self {
            openid: openid.into(),
            openid_file: None,
            http,
            api_base,
        }
    }

    /// 启用 openid 文件热加载（装配层传配置里的 openid_path）。
    pub fn with_openid_file(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.openid_file = Some(path.into());
        self
    }

    /// 每请求解析凭证：文件 > 构造字符串。文件是换绑入口，永远代表最新意图；
    /// 与 openid_from_all 的「配置 > 文件」顺序刻意不同——运行期以热更新为准。
    fn current_openid(&self) -> String {
        self.openid_file
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| self.openid.clone())
    }

    /// 平台按 Referer 区分入口页；对齐社区实现，指向学生签到页。
    fn referer(&self, path: &str) -> String {
        format!("{}{path}openid={}", self.api_base, self.openid)
    }

    /// GET + 网络抖动重试：windy2 实测到 v18 偶发连接失败（HTTP 000），
    /// 三次尝试（直连、+300ms、+1.2s）全败才交错。只用于幂等 GET；签到 POST 不重试。
    async fn get_with_retry(
        &self,
        url: String,
        referer_path: &str,
        what: &'static str,
    ) -> Result<(reqwest::StatusCode, String)> {
        let mut last: Option<anyhow::Error> = None;
        for delay_ms in [0u64, 300, 1200] {
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            let attempt = async {
                let response = self
                    .http
                    .get(&url)
                    .header("Openid", self.current_openid())
                    .header("Referer", self.referer(referer_path))
                    .send()
                    .await
                    .with_context(|| format!("{what}请求失败"))?;
                let status = response.status();
                let text = response
                    .text()
                    .await
                    .with_context(|| format!("{what}响应读取失败"))?;
                Ok((status, text))
            };
            match attempt.await {
                Ok(pair) => return Ok(pair),
                Err(err) => last = Some(err),
            }
        }
        Err(last.expect("retry loop ran at least once"))
    }

    /// 进行中的签到列表；openid 失效返回 Expired（轮询侧走每日告警，不重试）。
    pub async fn active_signs(&self) -> Result<SignsOutcome> {
        let (status, text) = self
            .get_with_retry(
                format!("{}{ACTIVE_SIGNS_PATH}", self.api_base),
                "/wechat-pro-ssr/student/sign?",
                "待签列表",
            )
            .await?;
        if is_expired(status, &text) {
            return Ok(SignsOutcome::Expired);
        }
        let signs: Vec<ActiveSign> = serde_json::from_str(&text)
            .with_context(|| format!("待签列表响应不是合法 JSON 数组：{}", truncate(&text)))?;
        Ok(SignsOutcome::Active(signs))
    }

    /// 签到（普通/GPS）。GPS 无坐标时也可发普通载荷（服务端校验弱，见 recon）。
    /// 返回原始响应文本，成败判定交 sign_response_ok（跨版本形状不一）。
    pub async fn sign_in(&self, sign: &ActiveSign, location: Option<(f64, f64)>) -> Result<String> {
        let mut body = serde_json::json!({
            "courseId": sign.course_id,
            "signId": sign.sign_id,
        });
        if let Some((lat, lon)) = location {
            body["lat"] = serde_json::json!(lat);
            body["lon"] = serde_json::json!(lon);
        }
        let response = self
            .http
            .post(format!("{}{SIGN_IN_PATH}", self.api_base))
            .header("Openid", self.current_openid())
            .header("Referer", self.referer("/wechat-pro-ssr/student/sign?"))
            .json(&body)
            .send()
            .await
            .context("签到请求失败")?;
        let status = response.status();
        let response = response.text().await.context("签到响应读取失败")?;
        if is_expired(status, &response) {
            bail!("openid 已失效（{EXPIRED_MARKER}）：`teachermate bind <链接>` 重新投喂");
        }
        Ok(response)
    }

    /// 学籍信息（bind 验证 openid + 身份回显）；失效返回带特征串的错误。
    pub async fn role(&self) -> Result<Vec<StudentRole>> {
        let (status, response) = self
            .get_with_retry(
                format!("{}{ROLE_PATH}", self.api_base),
                "/wechat-pro/student/archive/lists?",
                "学籍信息",
            )
            .await?;
        if is_expired(status, &response) {
            bail!("openid 已失效（{EXPIRED_MARKER}）");
        }
        let roles: Vec<StudentRole> = serde_json::from_str(&response)
            .with_context(|| format!("学籍信息响应不是合法 JSON 数组：{}", truncate(&response)))?;
        Ok(roles)
    }

    /// 有作业的课程列表（作业监控入口；端点与字段名逆向自 wechat-pro-ssr chunk 40）。
    pub async fn homework_courses(&self) -> Result<Vec<HomeworkCourse>> {
        let (status, text) = self
            .get_with_retry(
                format!("{}{HOMEWORK_COURSES_PATH}", self.api_base),
                "/wechat-pro-ssr/student/homework?",
                "作业课程列表",
            )
            .await?;
        if is_expired(status, &text) {
            bail!("openid 已失效（{EXPIRED_MARKER}）：`teachermate bind <链接>` 重新投喂");
        }
        let value: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("作业课程列表响应不是合法 JSON：{}", truncate(&text)))?;
        Ok(parse_homework_courses(&value))
    }

    /// 单门课程的作业清单（{homeworkList, assessments}，信封形状未实测，宽容解析）。
    pub async fn course_homeworks(&self, course_id: i64) -> Result<CourseHomework> {
        let (status, text) = self
            .get_with_retry(
                format!("{}{HOMEWORK_LIST_PATH}?courseId={course_id}", self.api_base),
                "/wechat-pro-ssr/student/homework?",
                "作业清单",
            )
            .await?;
        if is_expired(status, &text) {
            bail!("openid 已失效（{EXPIRED_MARKER}）：`teachermate bind <链接>` 重新投喂");
        }
        let value: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("作业清单响应不是合法 JSON：{}", truncate(&text)))?;
        Ok(parse_course_homework(&value))
    }
}

/// 平台签到响应的成败判定。响应形状跨版本不一（文档数组 ["OK",...]、
/// {"studentRank":..}、{"errorCode":..}），按特征串兼容：
/// 名次字段或 OK 数组或"签到成功"文案视为成功；"你已经签到成功"与
/// errorCode 305 视为已签（正向，同学习通语义）；其余按失败。
pub fn sign_response_ok(response: &str) -> bool {
    response.contains("studentRank")
        || response.contains("signRank")
        || response.contains("你已经签到成功")
        || response.contains("签到成功")
        || response.contains(r#""OK""#)
        || error_code_ok(response)
}

/// yatm 语义：errorCode 缺失/0 视为成功，305（已签）同样正向。
fn error_code_ok(response: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|value| value.get("errorCode").and_then(serde_json::Value::as_i64))
        .is_some_and(|code| code == 0 || code == 305)
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= 200 {
        text.to_owned()
    } else {
        let head: String = text.chars().take(200).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_response_covers_known_shapes() {
        // twchn 文档形状（数组，首元素 OK）
        assert!(sign_response_ok(
            r#"{"1222203":["OK",1,1,0,5,"2018-04-13 20:03:19.059",0,-1,5,5]}"#
        ));
        // wzj_sign_public 形状（studentRank）
        assert!(sign_response_ok(r#"{"signRank":2,"studentRank":7}"#));
        // 已签（正向）
        assert!(sign_response_ok("你已经签到成功"));
        // yatm errorCode 语义：0/缺失成功，305 已签
        assert!(sign_response_ok(r#"{"errorCode":305}"#));
        assert!(sign_response_ok(r#"{"errorCode":0}"#));
        // 失败
        assert!(!sign_response_ok(r#"{"errorCode":101}"#));
        assert!(!sign_response_ok("你不在我附近"));
        assert!(!sign_response_ok(""));
    }

    #[test]
    fn expired_marker_matches_platform_message() {
        let body = r#"{"message":"登录信息失效，请退出后重试"}"#;
        assert!(body.contains(EXPIRED_MARKER));
    }
}
