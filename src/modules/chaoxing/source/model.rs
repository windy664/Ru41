//! 学习通领域模型：签到方式、课程、活动、作业与种类识别。

use serde::Deserialize;
use std::path::PathBuf;

pub fn fallback_photo_bytes() -> &'static [u8] {
    include_bytes!("../assets/black.jpg")
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
            .unwrap_or_else(|| crate::modules::chaoxing::service::sign_type_name(self.sign_type).to_owned())
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
