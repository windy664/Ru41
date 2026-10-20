use crate::app::todo::TodoService;
use crate::ports::{channel::NotifyChannel, storage::TaskStore, vault::Vault};
use anyhow::{Context, Result};
use chrono::TimeZone;
use serde::Serialize;
use std::sync::Arc;

use super::source::ChaoxingClient;

/// vault 渲染缓存：(课程名, [(作业, 截止)]) 列表。
type CourseNotes = Vec<(String, Vec<(super::source::ChaoxingWork, Option<String>)>)>;

/// 学习通签到监控配置（对应配置文件的 [sources.chaoxing]）。
#[derive(Debug, Clone)]
pub struct ChaoxingConfig {
    pub phone: String,
    pub password: String,
    /// 会话 cookie 持久化路径。
    pub cookie_path: std::path::PathBuf,
    pub interval_secs: u64,
    /// 提醒投递目标（装配层按 [notify] 渠道解析）。
    pub notify_target: String,
    /// 自动签到：poll 发现待签活动时当场尝试（除二维码外全类型）。
    pub auto_sign: bool,
    /// 默认位置（坐标 + 地址文本）：位置/二维码签到的坐标缺省来源。
    pub default_location: Option<(f64, f64, String)>,
    /// 拍照签到默认照片路径；未配置且命令行未给 --photo 时用内置兜底照片。
    pub photo_path: Option<std::path::PathBuf>,
    /// 作业轮询间隔（秒）。
    pub work_poll_secs: u64,
    /// 截止前 N 天开始临期提醒。
    pub work_due_days: i64,
    /// 未交作业自动进待办、平台已交自动回勾。
    pub work_todo_sync: bool,
    /// 作业清单同步进 Obsidian 课程目录 + 跨平台作业总表。
    pub work_vault_sync: bool,
    /// Vault 内课程所在学期目录；None 表示不落 vault。
    pub vault_course_dir: Option<String>,
    /// 学习通课程名 → Vault 课程目录名映射。
    pub vault_courses: std::collections::BTreeMap<String, String>,
}

/// 签到子类型 → 中文名（与平台 nameOne 缺省时的兜底标签一致）。
pub fn sign_type_name(sign_type: Option<i64>) -> &'static str {
    match sign_type {
        Some(0) => "普通签到",
        Some(1) => "拍照签到",
        Some(2) => "二维码签到",
        Some(3) => "手势签到",
        Some(4) => "位置签到",
        Some(5) => "签到码签到",
        _ => "签到",
    }
}

/// 平台签到响应的成败判定：stuSignajax 成功返回含 success 的文本；
/// 「已签到/签到成功」视为正向（可能手机端已签，命令行补签无害）。
/// 其余（距离不足、参数错误等）按失败处理。
fn sign_response_ok(response: &str) -> bool {
    let text = response.to_lowercase();
    text.contains("success") || response.contains("签到成功") || response.contains("已签到")
}

/// 一轮轮询的报告。
#[derive(Debug, Serialize)]
pub struct ChaoxingPollReport {
    pub login_ok: bool,
    pub realname: Option<String>,
    pub courses: usize,
    pub sign_activities: usize,
    /// 进行中且未签的活动数。
    pub pending: usize,
    /// 本轮首次发现的签到活动数。
    pub new_activities: usize,
    /// 本轮自动签到成功的活动数（auto_sign 开启时）。
    pub auto_signed: usize,
    /// 本轮检测到「已签」变迁的活动数（含手机端/命令行签到）。
    pub signed: usize,
    /// 本轮检测到「已结束仍未签」的活动数。
    pub missed: usize,
    pub notified: bool,
}

/// 自动签到单次尝试的结果。
enum AutoSignOutcome {
    /// 平台接受：文案进回执。
    Signed(String),
    /// 平台明确拒绝或参数不全：标记已尝试、文案进回执、不再自动重试。
    Rejected(String),
    /// 网络等瞬时错误：不动快照，下轮自然重试。
    Transient,
}

/// 学习通签到监控：协议端登录 → 课程与签到活动枚举 →
/// 新活动落 records 去重 → 待签活动经默认出口提醒。
///
/// 通知策略：
/// - 待签活动（进行中且未签）首次发现 → auto_sign 开启时当场自动签（二维码除外，
///   enc 只能现场获取，回退为提醒）；关闭或二维码 → 提醒一次（含 activity_id）；
/// - 已结束/已签活动只入快照，不提醒；
/// - 登录态失效 → 每日最多提醒一次。
///
/// 自动签策略：平台接受 → ✅ 回执；平台明确拒绝 → 打「已尝试」标记、❌ 回执、
/// 不自动重试（避免对同一活动反复撞）；网络等瞬时错误 → 快照不动，下轮自然重试。
/// 所有签到动作均有 QQ 回执可审计。
pub struct ChaoxingTracker {
    store: Arc<dyn TaskStore>,
    notifier: Option<Arc<dyn NotifyChannel>>,
    client: ChaoxingClient,
    config: ChaoxingConfig,
}

impl ChaoxingTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        client: ChaoxingClient,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: ChaoxingConfig,
    ) -> Self {
        Self {
            store,
            notifier,
            client,
            config,
        }
    }

    /// 供 sign 子命令直接拿到已登录的客户端。
    pub fn client(&self) -> &ChaoxingClient {
        &self.client
    }

    /// 手动签到：按活动 ID 在全部课程中找到活动，按类型自动选择签到方式：
    /// - 普通/手势/签到码 → 直接签；
    /// - 位置 → 命令行坐标优先，否则用配置的默认位置（宿舍）；
    /// - 二维码 → 必须给 --enc（enc 是现场二维码里的时效 token，物理上无法凭空获得）；
    /// - 拍照 → --photo 指定照片，否则配置 photo_path，再否则用内置兜底照片。
    ///
    /// 签到结果（成败）经通知出口推送宿主；同时把 records 快照标记为已签，
    /// 避免下一轮 poll 把命令行签到再当状态变迁重复播报。
    /// 活动不在课程列表/已结束时返回错误，不做任何写回。
    pub async fn sign_manual(
        &self,
        activity_id: &str,
        lat: Option<f64>,
        lon: Option<f64>,
        addr: Option<&str>,
        enc: Option<&str>,
        photo_path: Option<&str>,
    ) -> Result<String> {
        self.client.ensure_session().await?;
        for course in self.client.course_list().await? {
            let Ok(activities) = self.client.activity_list(&course).await else {
                continue;
            };
            for activity in activities {
                if activity.id.to_string() != activity_id {
                    continue;
                }
                let method = self.resolve_method(&activity, lat, lon, addr, enc, photo_path)?;
                let response = self.client.sign(&course, &activity, &method).await?;
                let ok = sign_response_ok(&response);
                self.mark_signed(&course, &activity, "cli");
                if let Some(notifier) = &self.notifier {
                    let mark = if ok { "✅" } else { "❌" };
                    let message = format!(
                        "{mark} 学习通{}：{}｜{}（平台响应：{}）",
                        if ok { "签到成功" } else { "签到失败" },
                        course.title,
                        activity.type_label(),
                        response.trim()
                    );
                    notifier
                        .send(&self.config.notify_target, None, &message)
                        .await?;
                }
                return Ok(response);
            }
        }
        anyhow::bail!("找不到签到活动 {activity_id}（可能已结束、已下架或不在课程列表）");
    }

    /// 按活动类型 + 命令行参数 + 配置解析出具体签到方式。
    fn resolve_method(
        &self,
        activity: &super::source::ChaoxingActivity,
        lat: Option<f64>,
        lon: Option<f64>,
        addr: Option<&str>,
        enc: Option<&str>,
        photo_path: Option<&str>,
    ) -> Result<super::source::SignMethod> {
        use super::source::SignMethod;
        let default = self.config.default_location.as_ref();
        Ok(match activity.sign_type {
            // 位置：坐标与地址逐项回落到默认位置配置
            Some(4) => {
                let (dlat, dlon, daddr) = match default {
                    Some((lat, lon, addr)) => (Some(*lat), Some(*lon), Some(addr.clone())),
                    None => (None, None, None),
                };
                let lat = lat.or(dlat);
                let lon = lon.or(dlon);
                let addr = addr.map(str::to_owned).or(daddr);
                match (lat, lon, addr) {
                    (Some(lat), Some(lon), Some(addr)) => SignMethod::Location { lat, lon, addr },
                    _ => anyhow::bail!(
                        "位置签到需要坐标：给 --lat --lon --addr，或在配置里设 default_lat/default_lon/default_addr"
                    ),
                }
            }
            // 二维码：enc 只能来自现场二维码内容
            Some(2) => {
                let enc = enc.context(format!(
                    "二维码签到需要 --enc（现场扫二维码或扫照片得到的内容；{}）",
                    "enc 会过期，过期需重新获取"
                ))?;
                SignMethod::Qr {
                    enc: enc.to_owned(),
                    location: self.config.default_location.clone(),
                }
            }
            // 拍照：--photo > 配置 photo_path > 内置兜底照片
            Some(1) => {
                let path = match photo_path
                    .map(std::path::PathBuf::from)
                    .or_else(|| self.config.photo_path.clone())
                {
                    Some(path) => path,
                    None => self.materialize_fallback_photo()?,
                };
                SignMethod::Photo { path }
            }
            // 普通/手势/签到码及未知类型都走普通参数
            _ => SignMethod::Normal,
        })
    }

    /// 把内置兜底照片落到 cookie 同目录，返回其路径。
    fn materialize_fallback_photo(&self) -> Result<std::path::PathBuf> {
        let dir = self
            .config
            .cookie_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(dir)?;
        let path = dir.join("fallback-photo.jpg");
        std::fs::write(&path, super::source::fallback_photo_bytes())?;
        Ok(path)
    }

    /// 签到落定后更新快照：标记 user_status=0，poll 侧就不会再播一次「已签」变迁。
    fn mark_signed(
        &self,
        course: &super::source::ChaoxingCourse,
        activity: &super::source::ChaoxingActivity,
        via: &str,
    ) {
        let key = format!("{}#{}", course.course_id, activity.id);
        let now = crate::util::now();
        let result = match self.store.document("chaoxing_sign", &key) {
            Ok(Some(doc)) => {
                let mut value = doc.value.clone();
                value["user_status"] = serde_json::json!(0);
                value["signed_at"] = serde_json::json!(now);
                value["signed_via"] = serde_json::json!(via);
                self.store
                    .put_document("chaoxing_sign", &key, Some(doc.revision), &value)
            }
            Ok(None) => self.store.put_document(
                "chaoxing_sign",
                &key,
                None,
                &serde_json::json!({
                    "course": course.title,
                    "title": activity.type_label(),
                    "sign_type": activity.sign_type,
                    "start_at": format_ms(activity.start_time),
                    "end_at": format_ms(activity.end_time),
                    "status": activity.status,
                    "user_status": 0,
                    "first_seen": now,
                    "signed_at": now,
                    "signed_via": via,
                }),
            ),
            Err(err) => Err(err),
        };
        // 快照更新失败只影响去重精度（poll 可能补一条回执），不掩盖签到结果；
        // 仓库模块层不做日志，静默降级即可
        let _ = result;
    }

    /// 平台明确拒绝后打「已尝试」标记：自动签不会对同一活动反复重试。
    fn mark_auto_attempted(
        &self,
        course: &super::source::ChaoxingCourse,
        activity: &super::source::ChaoxingActivity,
        note: &str,
    ) {
        let key = format!("{}#{}", course.course_id, activity.id);
        let _ = self.store.document("chaoxing_sign", &key).map(|doc| {
            doc.map(|doc| {
                let mut value = doc.value.clone();
                value["auto_attempted"] = serde_json::json!(crate::util::now());
                value["auto_note"] = serde_json::json!(note);
                self.store
                    .put_document("chaoxing_sign", &key, Some(doc.revision), &value)
            })
        });
    }

    /// 自动签到一次：成功/拒绝给回执文案，瞬时错误静默（下轮重试）。
    async fn try_auto_sign(
        &self,
        course: &super::source::ChaoxingCourse,
        activity: &super::source::ChaoxingActivity,
    ) -> AutoSignOutcome {
        let method = match self.resolve_method(activity, None, None, None, None, None) {
            Ok(method) => method,
            Err(err) => {
                self.mark_auto_attempted(course, activity, &err.to_string());
                return AutoSignOutcome::Rejected(format!(
                    "- ❌{}｜{}｜自动签到配置不全：{err}",
                    course.title,
                    activity.type_label()
                ));
            }
        };
        match self.client.sign(course, activity, &method).await {
            Ok(response) => {
                if sign_response_ok(&response) {
                    self.mark_signed(course, activity, "auto");
                    AutoSignOutcome::Signed(format!(
                        "- ✅{}｜{}｜自动签到成功（平台响应：{}）",
                        course.title,
                        activity.type_label(),
                        response.trim()
                    ))
                } else {
                    self.mark_auto_attempted(course, activity, response.trim());
                    AutoSignOutcome::Rejected(format!(
                        "- ❌{}｜{}｜自动签到被平台拒绝（响应：{}）→ 可 `chaoxing sign {}` 重试",
                        course.title,
                        activity.type_label(),
                        response.trim(),
                        activity.id
                    ))
                }
            }
            // 网络等瞬时错误：快照不动，下一轮 poll 自然重试（窗口常为 1-2 分钟）
            Err(_) => AutoSignOutcome::Transient,
        }
    }

    pub async fn poll_once(&self) -> Result<ChaoxingPollReport> {
        let now = crate::util::now();
        let today = crate::util::today_utc(now);

        if let Err(err) = self.client.ensure_session().await {
            let key = format!("login:{today}");
            let nagged = self.store.document("chaoxing_alert", &key)?.is_some();
            let mut alerted = false;
            if !nagged && let Some(notifier) = self.notifier.as_ref() {
                self.store.put_document(
                    "chaoxing_alert",
                    &key,
                    None,
                    &serde_json::json!({ "ts": now }),
                )?;
                notifier
                    .send(
                        &self.config.notify_target,
                        None,
                        &format!(
                            "⚠️ 学习通登录态失效：{err}\n请检查 sources.chaoxing 凭证或稍后重试"
                        ),
                    )
                    .await?;
                alerted = true;
            }
            return Ok(ChaoxingPollReport {
                login_ok: false,
                realname: None,
                courses: 0,
                sign_activities: 0,
                pending: 0,
                new_activities: 0,
                auto_signed: 0,
                signed: 0,
                missed: 0,
                notified: alerted,
            });
        }

        let courses = self.client.course_list().await?;
        let mut sign_activities = 0usize;
        let mut new_activities = 0usize;
        let mut pending = 0usize;
        let mut auto_signed = 0usize;
        let mut signed = 0usize;
        let mut missed = 0usize;
        let mut pending_lines: Vec<String> = Vec::new();
        let mut outcome_lines: Vec<String> = Vec::new();

        for course in &courses {
            let Ok(activities) = self.client.activity_list(course).await else {
                // 单门课程活动列表失败只影响本轮该课程，不破坏整体轮询
                continue;
            };
            for activity in activities.into_iter().filter(|a| a.is_sign()) {
                sign_activities += 1;
                if activity.is_pending() {
                    pending += 1;
                }
                let key = format!("{}#{}", course.course_id, activity.id);
                let existing = self.store.document("chaoxing_sign", &key)?;
                let Some(doc) = existing else {
                    self.store.put_document(
                        "chaoxing_sign",
                        &key,
                        None,
                        &serde_json::json!({
                            "course": course.title,
                            "title": activity.type_label(),
                            "sign_type": activity.sign_type,
                            "start_at": format_ms(activity.start_time),
                            "end_at": format_ms(activity.end_time),
                            "status": activity.status,
                            "user_status": activity.user_status,
                            "first_seen": now,
                        }),
                    )?;
                    new_activities += 1;
                    // 待签活动：自动签开启则当场签（二维码除外，enc 只能现场获取）；
                    // 否则推送宿主，短窗口活动靠下一轮状态变迁收敛
                    if activity.is_pending() {
                        if self.config.auto_sign && activity.sign_type != Some(2) {
                            match self.try_auto_sign(course, &activity).await {
                                AutoSignOutcome::Signed(line) => {
                                    auto_signed += 1;
                                    outcome_lines.push(line);
                                }
                                AutoSignOutcome::Rejected(line) => outcome_lines.push(line),
                                AutoSignOutcome::Transient => {}
                            }
                        } else if self.notifier.is_some() {
                            pending_lines.push(format!(
                                "- ⏰{}｜{}｜进行中，截止 {}｜id={}｜`chaoxing sign {}`{}",
                                course.title,
                                activity.type_label(),
                                format_ms(activity.end_time).unwrap_or_else(|| "未知".into()),
                                activity.id,
                                activity.id,
                                if activity.sign_type == Some(2) {
                                    "（需 `--enc`，enc 来自现场二维码）"
                                } else {
                                    ""
                                },
                            ));
                        }
                    }
                    continue;
                };

                // 已见过：若自动签开启且仍未签、未被打过「已尝试」标记 → 补试一轮
                // （覆盖上次网络瞬时错误的自然重试；平台已拒绝的不会进来）
                let snapshot_user = doc
                    .value
                    .get("user_status")
                    .and_then(serde_json::Value::as_i64);
                if self.config.auto_sign
                    && activity.is_pending()
                    && snapshot_user == Some(1)
                    && activity.sign_type != Some(2)
                    && doc.value.get("auto_attempted").is_none()
                {
                    match self.try_auto_sign(course, &activity).await {
                        AutoSignOutcome::Signed(line) => {
                            auto_signed += 1;
                            outcome_lines.push(line);
                        }
                        AutoSignOutcome::Rejected(line) => outcome_lines.push(line),
                        AutoSignOutcome::Transient => {}
                    }
                    continue;
                }
                // 已见过：状态变迁才更新快照（乐观并发，更新成功才产出回执，天然去重）。
                // 只认两类有意义的变迁：未签→已签（✅），进行中→结束且未签（❌）；
                // 已签后的 status 收尾（如 (1,0)→(2,0)）不重复播报。
                let snapshot_status = doc.value.get("status").and_then(serde_json::Value::as_i64);
                if snapshot_status == activity.status && snapshot_user == activity.user_status {
                    continue;
                }
                let was_pending = snapshot_user == Some(1);
                let was_ongoing = snapshot_status == Some(1);
                let mut new_value = doc.value.clone();
                new_value["status"] = serde_json::json!(activity.status);
                new_value["user_status"] = serde_json::json!(activity.user_status);
                new_value["updated_at"] = serde_json::json!(now);
                let updated = self.store.put_document(
                    "chaoxing_sign",
                    &key,
                    Some(doc.revision),
                    &new_value,
                )?;
                if !updated {
                    continue;
                }
                if was_pending && activity.user_status != Some(1) {
                    signed += 1;
                    outcome_lines.push(format!(
                        "- ✅{}｜{}｜签到成功",
                        course.title,
                        activity.type_label()
                    ));
                } else if was_ongoing
                    && activity.status == Some(2)
                    && activity.user_status == Some(1)
                {
                    missed += 1;
                    outcome_lines.push(format!(
                        "- ❌{}｜{}｜已结束，仍未签到",
                        course.title,
                        activity.type_label()
                    ));
                }
            }
        }

        let mut notified = false;
        if let Some(notifier) = &self.notifier {
            if !pending_lines.is_empty() {
                let message = format!(
                    "【学习通签到】{}门课程 {} 个签到活动，{} 个待签\n{}\n（二维码需现场 enc，其余可 `chaoxing sign <id>` 手动签）",
                    courses.len(),
                    sign_activities,
                    pending,
                    pending_lines.join("\n")
                );
                notifier
                    .send(&self.config.notify_target, None, &message)
                    .await?;
                notified = true;
            }
            if !outcome_lines.is_empty() {
                let message = format!("【学习通签到回执】\n{}", outcome_lines.join("\n"));
                notifier
                    .send(&self.config.notify_target, None, &message)
                    .await?;
                notified = true;
            }
        }

        Ok(ChaoxingPollReport {
            login_ok: true,
            realname: self.client.realname().await,
            courses: courses.len(),
            sign_activities,
            pending,
            new_activities,
            auto_signed,
            signed,
            missed,
            notified,
        })
    }
}

/// 学习通作业监控报告。
#[derive(Debug, Serialize)]
pub struct ChaoxingWorkPollReport {
    pub login_ok: bool,
    pub courses: usize,
    pub works: usize,
    /// 本轮首次发现的未交作业数。
    pub new_opens: usize,
    /// 本轮新进入临期窗口的作业数。
    pub due_soon: usize,
    /// 当前已过期未交的作业数。
    pub overdue: usize,
    /// 本轮检测到「已交」变迁的作业数（自动回勾待办）。
    pub submitted: usize,
    pub todo_created: usize,
    pub vault_files: usize,
    pub notified: bool,
}

/// 学习通作业监控：复用签到模块的协议客户端（同一 cookie 会话），低频轮询：
/// 进课取 enc 三件套 → 作业列表 → 未交作业按截止分类 → 通知/待办/Obsidian。
///
/// 与智慧树作业模块（sources.zhihuishu）同构，并共享同一个 Vault 学期目录：
/// 每门课写 `学习通作业.md` 课程索引 + `{截止日}_{作业名}/作业说明.md`，
/// 再把两个平台的快照聚合成学期级 `作业总表.md`——后续新作业平台照此接入。
///
/// 红线：作业只做提醒/跟踪/归档，不做自动作答（自动答题=代写）。
/// 请求预算：每课 2 个只读请求（middle+list），详情页只对未交作业拉取；
/// 平台返回无权限/3004 时跳过该课并不刷屏（下轮重试）。
pub struct ChaoxingWorkTracker {
    store: Arc<dyn TaskStore>,
    notifier: Option<Arc<dyn NotifyChannel>>,
    client: ChaoxingClient,
    todos: TodoService,
    vault: Option<Arc<dyn Vault>>,
    config: ChaoxingConfig,
}

impl ChaoxingWorkTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        client: ChaoxingClient,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: ChaoxingConfig,
        todo_options: crate::domain::todo::TodoOptions,
        vault: Option<Arc<dyn Vault>>,
    ) -> Self {
        Self {
            todos: TodoService::new(store.clone(), todo_options),
            store,
            notifier,
            client,
            vault,
            config,
        }
    }

    pub async fn poll_once(&self) -> Result<ChaoxingWorkPollReport> {
        use crate::domain::todo::{TodoInput, TodoPriority, TodoSource};
        let now = crate::util::now();
        let today = crate::util::today_utc(now);
        let offset = self.todos.options().offset_seconds;

        if let Err(err) = self.client.ensure_session().await {
            // 与签到模块共用 login:{date} 告警键：同一会话一天最多告警一次
            let key = format!("login:{today}");
            let nagged = self.store.document("chaoxing_alert", &key)?.is_some();
            let mut notified = false;
            if !nagged && let Some(notifier) = self.notifier.as_ref() {
                self.store.put_document(
                    "chaoxing_alert",
                    &key,
                    None,
                    &serde_json::json!({ "ts": now }),
                )?;
                notifier
                    .send(
                        &self.config.notify_target,
                        None,
                        &format!("⚠️ 学习通登录态失效（作业轮询）：{err}"),
                    )
                    .await?;
                notified = true;
            }
            return Ok(ChaoxingWorkPollReport {
                login_ok: false,
                courses: 0,
                works: 0,
                new_opens: 0,
                due_soon: 0,
                overdue: 0,
                submitted: 0,
                todo_created: 0,
                vault_files: 0,
                notified,
            });
        }

        let courses = self.client.course_list().await?;
        let urgent_window = self.config.work_due_days * 86_400;
        let mut works_total = 0usize;
        let mut new_opens = 0usize;
        let mut due_soon = 0usize;
        let mut overdue_total = 0usize;
        let mut submitted = 0usize;
        let mut todo_created = 0usize;
        let mut lines: Vec<String> = Vec::new();
        // vault 渲染缓存：(课程名, [(作业, 截止)])
        let mut course_notes: CourseNotes = Vec::new();

        for course in &courses {
            let encs = match self.client.course_encs(course).await {
                Ok(encs) => encs,
                Err(_) => continue,
            };
            let works = match self.client.work_list(course, &encs).await {
                Ok(works) => works,
                Err(_) => continue, // 单课失败不破坏整体，也不刷屏
            };
            works_total += works.len();
            let mut note_items: Vec<(super::source::ChaoxingWork, Option<String>)> = Vec::new();

            for work in works {
                let key = format!("{}#{}", course.course_id, work.work_id);
                let open = work.is_open();
                let existing = self.store.document("chaoxing_work", &key)?;

                // 截止时间：快照缓存优先；未交且缺失才拉详情页（请求预算）
                let mut deadline = existing
                    .as_ref()
                    .and_then(|doc| doc.value.get("end_at"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .filter(|s| !s.is_empty());
                if open && deadline.is_none() {
                    deadline = self
                        .client
                        .work_deadline(&work.detail_url)
                        .await
                        .unwrap_or(None);
                }

                match existing {
                    None => {
                        let mut snapshot = serde_json::json!({
                            "course": course.title,
                            "title": work.title,
                            "status": work.status,
                            "end_at": deadline,
                            "first_seen": now,
                        });
                        self.store
                            .put_document("chaoxing_work", &key, None, &snapshot)?;
                        if open {
                            new_opens += 1;
                            lines.push(format!(
                                "- 📝{}｜{}｜{}｜截止 {}",
                                course.title,
                                work.title,
                                work.status,
                                deadline.as_deref().unwrap_or("未知"),
                            ));
                            if self.config.work_todo_sync && !work.status.contains("已过期") {
                                let end_ts = deadline
                                    .as_deref()
                                    .and_then(|s| platform_local_ts(s, offset));
                                let due_soon_flag = end_ts
                                    .map(|ts| ts - now <= urgent_window && ts > now)
                                    .unwrap_or(false);
                                match self.todos.add(
                                    TodoInput {
                                        title: format!("学习通｜{}｜{}", course.title, work.title),
                                        priority: if due_soon_flag {
                                            TodoPriority::High
                                        } else {
                                            TodoPriority::Normal
                                        },
                                        labels: vec!["学习通".into()],
                                        due_at: end_ts,
                                        remind_at: end_ts,
                                        source: Some(TodoSource {
                                            channel: "chaoxing".into(),
                                            source_id: Some(work.work_id.clone()),
                                        }),
                                    },
                                    now,
                                ) {
                                    Ok(item) => {
                                        snapshot["todo_id"] = serde_json::json!(item.id);
                                        // 回写 todo_id（CAS 失败仅影响回勾精度）
                                        if let Ok(Some(doc)) =
                                            self.store.document("chaoxing_work", &key)
                                        {
                                            let mut value = doc.value.clone();
                                            value["todo_id"] = serde_json::json!(item.id);
                                            let _ = self.store.put_document(
                                                "chaoxing_work",
                                                &key,
                                                Some(doc.revision),
                                                &value,
                                            );
                                        }
                                        todo_created += 1;
                                    }
                                    Err(err) => lines.push(format!(
                                        "- ⚠️{}｜{}｜待办创建失败: {}",
                                        course.title, work.title, err
                                    )),
                                }
                            }
                        }
                    }
                    Some(doc) => {
                        let prev_status = doc
                            .value
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let was_open = prev_status.contains('未');
                        if was_open && !open {
                            // 未交 → 已交：更新快照 + 回勾待办（一次性）
                            submitted += 1;
                            let mut value = doc.value.clone();
                            value["status"] = serde_json::json!(work.status);
                            value["updated_at"] = serde_json::json!(now);
                            let updated = self.store.put_document(
                                "chaoxing_work",
                                &key,
                                Some(doc.revision),
                                &value,
                            )?;
                            if updated {
                                lines.push(format!(
                                    "- ✅{}｜{}｜已提交（{}）",
                                    course.title, work.title, work.status
                                ));
                                if let Some(todo_id) =
                                    doc.value.get("todo_id").and_then(serde_json::Value::as_str)
                                {
                                    let _ = self.todos.complete(todo_id, now);
                                }
                            }
                        } else if open {
                            // 临期/过期每日最多提醒一次
                            let end_ts = deadline
                                .as_deref()
                                .and_then(|s| platform_local_ts(s, offset));
                            let is_overdue = end_ts.map(|ts| ts < now).unwrap_or(false);
                            let is_due_soon = end_ts
                                .map(|ts| ts - now <= urgent_window && ts > now)
                                .unwrap_or(false);
                            if is_overdue || is_due_soon {
                                let category = if is_overdue { "overdue" } else { "due" };
                                let alert_key = format!("{category}:{today}:{key}");
                                if self
                                    .store
                                    .document("chaoxing_work_alert", &alert_key)?
                                    .is_none()
                                {
                                    self.store.put_document(
                                        "chaoxing_work_alert",
                                        &alert_key,
                                        None,
                                        &serde_json::json!({ "ts": now }),
                                    )?;
                                    if is_overdue {
                                        overdue_total += 1;
                                        lines.push(format!(
                                            "- ⚠️{}｜{}｜已过期未交（截止 {}）",
                                            course.title,
                                            work.title,
                                            deadline.as_deref().unwrap_or("未知"),
                                        ));
                                    } else {
                                        due_soon += 1;
                                        lines.push(format!(
                                            "- ⏰{}｜{}｜{}内截止（{}）",
                                            course.title,
                                            work.title,
                                            self.config.work_due_days,
                                            deadline.as_deref().unwrap_or("未知"),
                                        ));
                                    }
                                }
                            }
                            // 截止时间若首次拿到，回写快照
                            if doc.value.get("end_at").and_then(serde_json::Value::as_str)
                                != deadline.as_deref()
                                && let Some(d) = deadline.as_deref()
                            {
                                let mut value = doc.value.clone();
                                value["end_at"] = serde_json::json!(d);
                                let _ = self.store.put_document(
                                    "chaoxing_work",
                                    &key,
                                    Some(doc.revision),
                                    &value,
                                );
                            }
                        }
                    }
                }
                note_items.push((work, deadline));
            }
            course_notes.push((course.title.clone(), note_items));
        }

        let vault_files = self.sync_vault(&course_notes, now)?;

        let mut notified = false;
        if !lines.is_empty()
            && let Some(notifier) = self.notifier.as_ref()
        {
            let mut message = format!("【学习通作业】\n{}", lines.join("\n"));
            if vault_files > 0 {
                message.push_str(&format!("\n（已同步 {vault_files} 个笔记到 Obsidian）"));
            }
            notifier
                .send(&self.config.notify_target, None, &message)
                .await?;
            notified = true;
        }

        Ok(ChaoxingWorkPollReport {
            login_ok: true,
            courses: courses.len(),
            works: works_total,
            new_opens,
            due_soon,
            overdue: overdue_total,
            submitted,
            todo_created,
            vault_files,
            notified,
        })
    }

    /// 作业清单落 Vault：每课 `学习通作业.md` + 作业文件夹 `作业说明.md`，
    /// 再聚合两个平台快照写学期级 `作业总表.md`。内容无变化不提交。
    fn sync_vault(&self, course_notes: &CourseNotes, now: i64) -> Result<usize> {
        let Some(vault) = &self.vault else {
            return Ok(0);
        };
        let Some(base) = self
            .config
            .vault_course_dir
            .as_deref()
            .map(|s| s.trim_end_matches('/'))
        else {
            return Ok(0);
        };
        let offset = self.todos.options().offset_seconds;
        let mut written = 0usize;
        for (course_title, items) in course_notes {
            if items.is_empty() {
                continue;
            }
            let dir = self
                .config
                .vault_courses
                .get(course_title.as_str())
                .cloned()
                .unwrap_or_else(|| course_title.clone());
            let course_base = format!("{base}/{dir}");
            vault.write_file(
                &format!("{course_base}/学习通作业.md"),
                &render_work_course_note(course_title, items, now, offset),
            )?;
            written += 1;
            for (work, deadline) in items {
                let folder = work_folder_name(work, deadline);
                vault.write_file(
                    &format!("{course_base}/{folder}/作业说明.md"),
                    &render_work_task_note(course_title, work, deadline, now, offset),
                )?;
                written += 1;
            }
        }
        // 跨平台聚合：学习通 + 智慧树快照 → 学期级作业总表
        vault.write_file(
            &format!("{base}/作业总表.md"),
            &self.render_homework_dashboard(now, offset)?,
        )?;
        written += 1;
        if vault.commit_all("[ru41] chaoxing 课程作业同步")? {
            vault.push()?;
            Ok(written)
        } else {
            Ok(0) // 内容无变化，不提交不播报
        }
    }

    /// 跨平台作业总表：从 records 快照聚合（新平台接入时在此加一行映射）。
    fn render_homework_dashboard(&self, now: i64, offset_seconds: i32) -> Result<String> {
        let mut rows: Vec<(Option<String>, String, String, String, String, bool)> = Vec::new();
        // (end_at, platform, course_dir, course, title, open)
        for doc in self.store.documents("chaoxing_work", 500)? {
            let course = str_field(&doc.value, "course").unwrap_or_default();
            let title = str_field(&doc.value, "title").unwrap_or_default();
            let status = str_field(&doc.value, "status").unwrap_or_default();
            let end_at = str_field(&doc.value, "end_at");
            let open = status.contains('未');
            rows.push((end_at, "学习通".into(), course.clone(), course, title, open));
        }
        for doc in self.store.documents("zhihuishu_task", 500)? {
            let course = str_field(&doc.value, "course").unwrap_or_default();
            let title = str_field(&doc.value, "title").unwrap_or_default();
            let end_at = str_field(&doc.value, "end_at");
            let completed = doc
                .value
                .get("completed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            rows.push((
                end_at,
                "智慧树".into(),
                course.clone(),
                course,
                title,
                !completed,
            ));
        }
        // 未完成在前按截止升序（空截止垫底），已完成按截止降序
        rows.sort_by(|a, b| b.5.cmp(&a.5).then(a.0.cmp(&b.0)));

        let stamp = local_stamp(now, offset_seconds);
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str("tags:\n  - 作业\n  - 作业总表\n");
        out.push_str(&format!("updated: {stamp}\n---\n\n"));
        out.push_str("# 作业总表\n\n");
        out.push_str(
            "> 由 Ru41 跨平台聚合自动覆写（学习通 + 智慧树，新平台随接入追加）；请勿手改。\n\n",
        );
        let open: Vec<_> = rows.iter().filter(|r| r.5).collect();
        let done: Vec<_> = rows.iter().filter(|r| !r.5).collect();
        out.push_str(&format!("## 未完成（{}）\n\n", open.len()));
        if open.is_empty() {
            out.push_str("当前没有未完成作业 🎉\n");
        }
        for (end_at, platform, dir, course, title, _) in &open {
            let overdue = end_at
                .as_deref()
                .and_then(|s| platform_local_ts(s, offset_seconds))
                .is_some_and(|ts| ts < now);
            let mark = if overdue { "⚠️已过期 " } else { "" };
            out.push_str(&format!(
                "- [ ] {mark}{platform}｜{course}｜{title} 📅 {}\n",
                end_at.as_deref().unwrap_or("未知")
            ));
            let _ = dir;
        }
        if !done.is_empty() {
            out.push_str(&format!("\n## 已完成（{}）\n\n", done.len()));
            for (end_at, platform, _, course, title, _) in &done {
                out.push_str(&format!(
                    "- [x] {platform}｜{course}｜{title} 📅 {}\n",
                    end_at
                        .as_deref()
                        .map(|s| s.get(..10).unwrap_or(s))
                        .unwrap_or("未知")
                ));
            }
        }
        Ok(out)
    }
}

/// 平台时间串 "YYYY-MM-DD HH:MM"（北京时间）→ 本机固定偏移时间戳。
fn platform_local_ts(raw: &str, offset_seconds: i32) -> Option<i64> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%d %H:%M").ok()?;
    chrono::FixedOffset::east_opt(offset_seconds)?
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.timestamp())
}

/// 用户时区（固定偏移，容器安全）的 "%Y-%m-%d %H:%M" 时间戳。
fn local_stamp(now: i64, offset_seconds: i32) -> String {
    chrono::FixedOffset::east_opt(offset_seconds)
        .and_then(|tz| tz.timestamp_opt(now, 0).single())
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// 作业文件夹名：`YYYY.MM.DD_作业名`（日期取截止日，缺省用作业名）。
fn work_folder_name(work: &super::source::ChaoxingWork, deadline: &Option<String>) -> String {
    let date = deadline
        .as_deref()
        .and_then(|s| s.get(..10))
        .map(|d| d.replace('-', "."));
    let title: String = work
        .title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '－'
            } else {
                c
            }
        })
        .collect();
    match date {
        Some(d) => format!("{d}_{title}"),
        None => title,
    }
}

/// 课程级索引：`学习通作业.md`（未完成/已完成清单，与智慧树作业.md 同构）。
fn render_work_course_note(
    course: &str,
    items: &[(super::source::ChaoxingWork, Option<String>)],
    now: i64,
    offset_seconds: i32,
) -> String {
    let stamp = local_stamp(now, offset_seconds);
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {course}\n"));
    out.push_str("platform: 学习通\n");
    out.push_str(&format!("updated: {stamp}\n"));
    out.push_str("tags:\n  - 课程\n  - 学习通\n---\n\n");
    out.push_str(&format!("# {course} · 学习通作业\n\n"));
    out.push_str("> 由 Ru41 每轮轮询自动覆写，请勿手改；截止时间为平台北京时间。\n\n");
    let open: Vec<_> = items.iter().filter(|(w, _)| w.is_open()).collect();
    let done: Vec<_> = items.iter().filter(|(w, _)| !w.is_open()).collect();
    out.push_str(&format!("## 未完成（{}）\n\n", open.len()));
    if open.is_empty() {
        out.push_str("当前没有未完成作业 🎉\n");
    }
    for (work, deadline) in &open {
        let folder = work_folder_name(work, deadline);
        out.push_str(&format!(
            "- [ ] [[{folder}/作业说明|{}]] 📅 {}\n",
            work.title,
            deadline.as_deref().unwrap_or("未知")
        ));
    }
    if !done.is_empty() {
        out.push_str(&format!("\n## 已完成（{}）\n\n", done.len()));
        for (work, deadline) in &done {
            let folder = work_folder_name(work, deadline);
            let date = deadline
                .as_deref()
                .and_then(|s| s.get(..10))
                .unwrap_or("未知");
            out.push_str(&format!(
                "- [x] [[{folder}/作业说明|{}]] 📅 {date}\n",
                work.title
            ));
        }
    }
    out
}

/// 作业说明页：元信息 frontmatter + 平台链接（说明文字 v1 不抓取正文）。
fn render_work_task_note(
    course: &str,
    work: &super::source::ChaoxingWork,
    deadline: &Option<String>,
    now: i64,
    offset_seconds: i32,
) -> String {
    let stamp = local_stamp(now, offset_seconds);
    let status = if work.is_open() { "open" } else { "done" };
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {course}\n"));
    out.push_str(&format!("task: {}\n", work.title));
    if let Some(end) = deadline {
        out.push_str(&format!("due: \"{end}\"\n"));
    }
    out.push_str(&format!("status: {status}\n"));
    out.push_str(&format!("updated: {stamp}\n"));
    out.push_str("tags:\n  - 课程\n  - 学习通\n  - 作业\n---\n\n");
    out.push_str(&format!("# {}\n\n", work.title));
    out.push_str(&format!("- 课程：{course}\n"));
    if let Some(end) = deadline {
        out.push_str(&format!("- 截止：{end}（北京时间）\n"));
    }
    out.push_str(&format!("- 状态：{}\n", work.status));
    out.push_str(&format!(
        "\n[平台作业页]({})\n\n> 由 Ru41 自学习通作业列表自动生成与更新。\n",
        work.detail_url
    ));
    out
}
fn format_ms(ms: Option<i64>) -> Option<String> {
    let ms = ms?;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
}

impl crate::ingress::schedule::PollJob for ChaoxingTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        Box::pin(async move { Ok(serde_json::to_value(self.poll_once().await?)?) })
    }
}

impl crate::ingress::schedule::PollJob for ChaoxingWorkTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.work_poll_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        Box::pin(async move { Ok(serde_json::to_value(self.poll_once().await?)?) })
    }
}
