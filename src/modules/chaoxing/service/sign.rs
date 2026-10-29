//! 学习通签到监控：轮询活动 → 待签当场自动签（全类型）→ 状态变迁回执。

use crate::modules::chaoxing::config::ChaoxingConfig;
use crate::modules::chaoxing::source::ChaoxingClient;
use crate::ports::{channel::NotifyChannel, storage::TaskStore};
use anyhow::Result;
use chrono::TimeZone;
use serde::Serialize;
use std::sync::Arc;

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
pub(super) fn sign_response_ok(response: &str) -> bool {
    let text = response.to_lowercase();
    text.contains("success") || response.contains("签到成功") || response.contains("已签到")
}

/// 课表窗口向前后各延伸的秒数（覆盖提前签到与拖堂后补签）。
const CLASS_WINDOW_EXTEND_SECS: i64 = 15 * 60;
/// 课表窗口外的兜底实轮询间隔（防课表外的临时签到漏检，同时压住请求量）。
const OFF_WINDOW_POLL_SECS: i64 = 30 * 60;

/// 一轮轮询的报告。
#[derive(Debug, Serialize)]
pub struct ChaoxingPollReport {    pub login_ok: bool,
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
    /// 本轮是否因不在课表窗口而跳过（未发任何网络请求）。
    pub window_skipped: bool,
    /// 活动列表拉取失败的课程数（限流/接口变更时 >0）；>0 说明监控部分或全部失明。
    pub course_errors: usize,
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
/// - 待签活动（进行中且未签）首次发现 → auto_sign 开启时当场自动签（二维码除外：
///   enc 只在二维码图片里，提醒把照片发给机器人，由 service::qr 消费签入）；
///   关闭或二维码 → 提醒一次（含 activity_id）；
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

    /// 当前是否处于课表窗口内（某节课 前 15 分钟 ~ 下课 后 15 分钟）。
    /// 课表来自 jwxt 同步落库的 event 记录（每节课一条独立实例，无 recurrence）。
    fn in_class_window(&self, now: i64) -> Result<bool> {
        let mut windows = Vec::new();
        for doc in self.store.documents("event", 10_000)? {
            let Ok(item) =
                serde_json::from_value::<crate::domain::schedule::EventItem>(doc.value.clone())
            else {
                continue;
            };
            if item.status != crate::domain::schedule::EventStatus::Active {
                continue;
            }
            if item.source.as_ref().map(|s| s.channel.as_str()) != Some("jwxt") {
                continue;
            }
            windows.push((
                item.starts_at - CLASS_WINDOW_EXTEND_SECS,
                item.ends_at + CLASS_WINDOW_EXTEND_SECS,
            ));
        }
        Ok(in_any_window(now, &windows))
    }

    /// 窗口外每 30 分钟允许一轮实轮询（以进入实轮询记账，失败也占用配额）。
    fn off_window_poll_due(&self, now: i64) -> Result<bool> {
        let last = self
            .store
            .document("chaoxing_meta", "last_full_poll")?
            .and_then(|d| d.value.get("ts").and_then(serde_json::Value::as_i64));
        Ok(last.is_none_or(|ts| now - ts >= OFF_WINDOW_POLL_SECS))
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
        activity: &crate::modules::chaoxing::source::ChaoxingActivity,
        lat: Option<f64>,
        lon: Option<f64>,
        addr: Option<&str>,
        enc: Option<&str>,
        photo_path: Option<&str>,
    ) -> Result<crate::modules::chaoxing::source::SignMethod> {
        use crate::modules::chaoxing::source::{ChaoxingSignKind, SignMethod};
        let default = self.config.default_location.as_ref();
        Ok(match activity.sign_kind() {
            // 位置：坐标与地址逐项回落到默认位置配置
            ChaoxingSignKind::Location => {
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
            // 二维码：enc 由服务端生成、只随二维码图片分发，平台校验真实性
            // （伪造值被「签到失败，请重新扫描。」拒绝，2026-10-23 实测）。
            // 缺 enc 直接报错并指路图片通道（service::qr）。
            ChaoxingSignKind::Qr => {
                let Some(enc) = enc.map(str::to_owned) else {
                    anyhow::bail!(
                        "二维码签到需要真实 enc：把二维码照片发给机器人（自动识别代签），或 CLI 传 --enc"
                    )
                };
                SignMethod::Qr {
                    enc,
                    location: self.config.default_location.clone(),
                }
            }
            // 拍照：--photo > 配置 photo_path > 内置兜底照片
            ChaoxingSignKind::Photo => {
                let path = match photo_path
                    .map(std::path::PathBuf::from)
                    .or_else(|| self.config.photo_path.clone())
                {
                    Some(path) => path,
                    None => self.materialize_fallback_photo()?,
                };
                SignMethod::Photo { path }
            }
            // 普通/手势/签到码及未知类型都走普通参数（平台不校验额外参数）
            ChaoxingSignKind::Normal | ChaoxingSignKind::Gesture | ChaoxingSignKind::Code => {
                SignMethod::Normal
            }
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
        std::fs::write(
            &path,
            crate::modules::chaoxing::source::fallback_photo_bytes(),
        )?;
        Ok(path)
    }

    /// 签到落定后更新快照：标记 user_status=0，poll 侧就不会再播一次「已签」变迁。
    fn mark_signed(
        &self,
        course: &crate::modules::chaoxing::source::ChaoxingCourse,
        activity: &crate::modules::chaoxing::source::ChaoxingActivity,
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
        course: &crate::modules::chaoxing::source::ChaoxingCourse,
        activity: &crate::modules::chaoxing::source::ChaoxingActivity,
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
    /// 科研线 B：窗口探测后台化（详情页 + preSign 找 enc 泄漏）。
    /// 失败只进 stderr，绝不影响轮询与提醒。
    fn spawn_enc_probe(&self, activity_id: i64) {
        let Some(path) = self.config.enc_sample_path.clone() else {
            return;
        };
        let client = self.client.clone();
        tokio::spawn(async move {
            super::enc_sample::probe_window(&client, activity_id, &path).await;
        });
    }

    async fn try_auto_sign(
        &self,
        course: &crate::modules::chaoxing::source::ChaoxingCourse,
        activity: &crate::modules::chaoxing::source::ChaoxingActivity,
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
                window_skipped: false,
                course_errors: 0,
                notified: alerted,
            });
        }

        // 课表窗口自适应轮询（2026-10-29 限流教训：activelist 全天候 60s 轮询
        // 会被平台「请勿频繁操作」掐死，且失败被静默跳过导致整天失明）。
        // 窗口 = 当天课表每节课 前后各 15 分钟；窗口内正常轮询；窗口外完全不发
        // 网络请求，仅每 30 分钟兜底一轮，防课表外的临时签到漏检。
        if !self.in_class_window(now)? && !self.off_window_poll_due(now)? {
            return Ok(ChaoxingPollReport {
                login_ok: true,
                realname: None,
                courses: 0,
                sign_activities: 0,
                pending: 0,
                new_activities: 0,
                auto_signed: 0,
                signed: 0,
                missed: 0,
                window_skipped: true,
                course_errors: 0,
                notified: false,
            });
        }
        // 进入实轮询即记账：失败也占用兜底配额，限流时不会每 30 分钟反复撞。
        self.store.put_document(
            "chaoxing_meta",
            "last_full_poll",
            None,
            &serde_json::json!({ "ts": now }),
        )?;

        let courses = self.client.course_list().await?;
        let mut sign_activities = 0usize;
        let mut new_activities = 0usize;
        let mut pending = 0usize;
        let mut auto_signed = 0usize;
        let mut signed = 0usize;
        let mut missed = 0usize;
        let mut pending_lines: Vec<String> = Vec::new();
        let mut outcome_lines: Vec<String> = Vec::new();
        let mut course_errors = 0usize;
        let mut last_course_error: Option<String> = None;

        for course in &courses {
            let activities = match self.client.activity_list(course).await {
                Ok(activities) => activities,
                Err(err) => {
                    // 单门课程失败只影响本轮该课程，但必须计数 surfaced 到报告：
                    // 全部失败（如平台限流「请勿频繁操作」）时监控等同失明，不能静默。
                    course_errors += 1;
                    last_course_error = Some(format!("{err:#}"));
                    continue;
                }
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
                    // 待签活动：自动签开启则当场签（二维码除外——enc 只在二维码
                    // 图片里，提醒用户把照片发给机器人，由 service::qr 消费签入）；
                    // 否则推送宿主，短窗口活动靠下一轮状态变迁收敛
                    if activity.is_pending() {
                        if self.config.auto_sign && !activity.is_qr() {
                            match self.try_auto_sign(course, &activity).await {
                                AutoSignOutcome::Signed(line) => {
                                    auto_signed += 1;
                                    outcome_lines.push(line);
                                }
                                AutoSignOutcome::Rejected(line) => outcome_lines.push(line),
                                AutoSignOutcome::Transient => {}
                            }
                        } else {
                            if activity.is_qr() {
                                // 科研线 B：窗口期间侦测学生端接口是否泄漏当前 enc
                                self.spawn_enc_probe(activity.id);
                            }
                            if self.notifier.is_some() {
                                pending_lines.push(format!(
                                    "- ⏰{}｜{}｜进行中，截止 {}｜id={}｜`chaoxing sign {}`{}",
                                    course.title,
                                    activity.type_label(),
                                    format_ms(activity.end_time).unwrap_or_else(|| "未知".into()),
                                    activity.id,
                                    activity.id,
                                    if activity.is_qr() {
                                        "（把二维码照片发给机器人即可代签）"
                                    } else {
                                        ""
                                    },
                                ));
                            }
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
        // 全部课程活动列表都拉取失败（登录态却正常）：签到监控处于失明状态，
        // 典型诱因是平台限流（「请勿频繁操作」）或接口变更。每日最多告警一次。
        if !courses.is_empty() && course_errors == courses.len() {
            let key = format!("activity_fail:{today}");
            if self.store.document("chaoxing_alert", &key)?.is_none()
                && let Some(notifier) = self.notifier.as_ref()
            {
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
                            "⚠️ 学习通签到监控失明：全部 {} 门课程的活动列表拉取失败（{}）。\n疑似平台限流或接口变更，签到检测当前不可用。",
                            courses.len(),
                            last_course_error.as_deref().unwrap_or("未知错误"),
                        ),
                    )
                    .await?;
                notified = true;
            }
        }
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
            window_skipped: false,
            course_errors,
            notified,
        })
    }
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

/// 纯函数窗口判定（边界含端点），抽出来便于单测。
fn in_any_window(now: i64, windows: &[(i64, i64)]) -> bool {
    windows.iter().any(|(start, end)| *start <= now && now <= *end)
}

#[cfg(test)]
mod tests {
    use super::in_any_window;

    #[test]
    fn class_window_boundaries() {
        let windows = [(1000, 2000)];
        assert!(!in_any_window(999, &windows));
        assert!(in_any_window(1000, &windows));
        assert!(in_any_window(1500, &windows));
        assert!(in_any_window(2000, &windows));
        assert!(!in_any_window(2001, &windows));
        assert!(!in_any_window(1500, &[]));
        assert!(in_any_window(1500, &[(3000, 4000), (1000, 2000)]));
    }
}
