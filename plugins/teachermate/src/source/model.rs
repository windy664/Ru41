//! 微助教领域模型：进行中的签到与学籍信息。

use serde::Deserialize;

/// 进行中的签到（active_signs 数组条目）。平台无 user_status 字段——
/// 是否已签只能通过实际发起 sign_in 的响应得知，与学习通协议不同。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ActiveSign {
    #[serde(rename = "courseId", default)]
    pub course_id: i64,
    #[serde(rename = "signId", default)]
    pub sign_id: i64,
    /// 1 = 需要 GPS 定位（服务端校验弱：普通载荷也常被接受，见 recon）。
    #[serde(rename = "isGPS", default)]
    pub is_gps: i64,
    /// 1 = 二维码签到（服务端轮换二维码，v1 不做替扫）。
    #[serde(rename = "isQR", default)]
    pub is_qr: i64,
    /// 课程名。
    #[serde(default)]
    pub name: String,
    /// 课堂编码。
    #[serde(default)]
    pub code: String,
}

impl ActiveSign {
    pub fn is_gps(&self) -> bool {
        self.is_gps != 0
    }
    pub fn is_qr(&self) -> bool {
        self.is_qr != 0
    }
    /// 签到种类标签：微助教只有普通/GPS/二维码三种。
    pub fn kind_label(&self) -> &'static str {
        if self.is_qr() {
            "二维码签到"
        } else if self.is_gps() {
            "位置签到"
        } else {
            "普通签到"
        }
    }
}

/// 学籍信息（/v2/students/role 数组条目）；bind 验证与身份回显用。
/// 字段全部 Option：平台各校数据填充不一致，缺字段不应让验证失败。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StudentRole {
    #[serde(default)]
    pub college_name: Option<String>,
    #[serde(default)]
    pub department_name: Option<String>,
    #[serde(default)]
    pub class_name: Option<String>,
    #[serde(default)]
    pub student_number: Option<String>,
}

/// 微助教作业状态标签（微信端 UI 状态表，2026-10 从 wechat-pro-ssr chunk 抠出）。
pub fn homework_status_label(status: i64) -> &'static str {
    match status {
        1 => "未完成",
        2 => "被打回",
        3 => "已超时",
        4 => "已提交", // 尚未评分，可修改
        5 => "已提交", // 评分中
        6 => "已公布成绩",
        7 => "补交中", // 老师允许补交
        8 => "已批阅",
        _ => "未知",
    }
}

/// 需要动作的作业状态：1 未完成 / 2 被打回 / 7 补交中。
/// 其余（已超时/已提交/评分中/已公布/已批阅）视为收敛。
pub fn homework_status_open(status: i64) -> bool {
    matches!(status, 1 | 2 | 7)
}

/// 作业条目（`/v1/homework?courseId=` 的 homeworkList 元素）。
/// 平台响应 schema 未完整实测（字段名来自前端 chunk），宽容提取：
/// 字符串/数字 id 都接受，除 id/name 外缺字段不致命。
#[derive(Debug, Clone, PartialEq)]
pub struct Homework {
    /// 作业 id（即详情/提交接口里的 hId）。
    pub h_id: i64,
    pub name: String,
    pub status: i64,
    /// 截止时间原文（平台格式未实测；解析见 service::work::deadline_ts）。
    pub deadline: Option<String>,
    pub submit_time: Option<String>,
}

impl Homework {
    pub fn status_label(&self) -> &'static str {
        homework_status_label(self.status)
    }
    pub fn is_open(&self) -> bool {
        homework_status_open(self.status)
    }

    /// 从原始 JSON 宽容提取；无 id 或 name 的条目丢弃。
    pub fn from_value(value: &serde_json::Value) -> Option<Homework> {
        let id = value.get("id").and_then(json_i64)?;
        let name = value
            .get("name")
            .and_then(serde_json::Value::as_str)?
            .to_owned();
        Some(Self {
            h_id: id,
            name,
            status: value.get("status").and_then(json_i64).unwrap_or(0),
            deadline: str_field(value, "deadline"),
            submit_time: str_field(value, "submitTime"),
        })
    }
}

/// 有作业的课程（`/v1/homework/courses` 条目）。
#[derive(Debug, Clone, PartialEq)]
pub struct HomeworkCourse {
    pub course_id: i64,
    pub course: String,
}

/// 单门课程的作业清单（`/v1/homework?courseId=` 响应）。
/// 信封形状未实测：{homeworkList, assessments} 或裸数组都接受。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CourseHomework {
    pub homeworks: Vec<Homework>,
    /// 互评任务数（v1 只计数不提醒，见 docs/sources-teachermate.md 边界）。
    pub assessments: usize,
}

/// 作业课程列表宽容解析：顶层数组、{data: [...]}、{courses: [...]}
/// （2026-10-24 实测为 {"courses":[]}）都接受。
pub fn parse_homework_courses(value: &serde_json::Value) -> Vec<HomeworkCourse> {
    let items = value
        .as_array()
        .or_else(|| value.get("data").and_then(serde_json::Value::as_array))
        .or_else(|| value.get("courses").and_then(serde_json::Value::as_array));
    let Some(items) = items else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let course_id = item.get("id").and_then(json_i64)?;
            let course = item
                .get("name")
                .and_then(serde_json::Value::as_str)
                .or_else(|| item.get("courseName").and_then(serde_json::Value::as_str))?
                .to_owned();
            Some(HomeworkCourse { course_id, course })
        })
        .collect()
}

/// 单课作业清单宽容解析：{homeworkList, assessments} 信封或裸数组。
pub fn parse_course_homework(value: &serde_json::Value) -> CourseHomework {
    let (list, assessments) = if let Some(list) = value.get("homeworkList") {
        (
            list,
            value
                .get("assessments")
                .and_then(serde_json::Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0),
        )
    } else if let Some(list) = value.get("data").and_then(|d| d.get("homeworkList")) {
        (
            list,
            value
                .get("data")
                .and_then(|d| d.get("assessments"))
                .and_then(serde_json::Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0),
        )
    } else if let Some(list) = value.as_array() {
        // 裸数组：Value 借用需要拥有化，转成临时引用路径
        let owned = serde_json::Value::Array(list.clone());
        return CourseHomework {
            homeworks: owned
                .as_array()
                .map(|a| a.iter().filter_map(Homework::from_value).collect())
                .unwrap_or_default(),
            assessments: 0,
        };
    } else {
        return CourseHomework::default();
    };
    CourseHomework {
        homeworks: list
            .as_array()
            .map(|a| a.iter().filter_map(Homework::from_value).collect())
            .unwrap_or_default(),
        assessments,
    }
}

/// 数字或数字字符串都接受的 i64 提取。
fn json_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

impl StudentRole {
    /// 一行可读身份摘要：学校｜学院｜班级｜学号（缺省段跳过）。
    pub fn summary(&self) -> String {
        [
            self.college_name.as_deref(),
            self.department_name.as_deref(),
            self.class_name.as_deref(),
            self.student_number.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join("｜")
    }
}

/// active_signs 的两种结局：正常数组，或 openid 失效（特征串命中）。
/// 失效不是网络错误，轮询侧要走「每日一次告警」路径而不是普通重试。
#[derive(Debug, Clone, PartialEq)]
pub enum SignsOutcome {
    Active(Vec<ActiveSign>),
    Expired,
}
