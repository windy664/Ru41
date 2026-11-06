//! 学习通签到监控：轮询活动 → 待签处理（普通/手势/位置/拍照/签到码当场自动签，
//! 被 90002 风控拦下时自动过滑块重签；二维码指路图片通道）→ 状态变迁回执。

use crate::config::ChaoxingConfig;
use crate::source::ChaoxingClient;
use anyhow::Result;
use chrono::TimeZone;
use ru41::ports::{channel::NotifyChannel, storage::TaskStore};
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

/// 签到页是否含拍照（图片）签到组件。
///
/// 平台的图片签到页面会带这几个特征串：拍照入口 `newOpenPhoto`、照片容器
/// `photoAreaId`、选图回调 `CLIENT_CHOOSE_IMAGE_RESULT`、提交值 `PHOTO_RESULT`。
/// 普通签到与手势签到的页面都不含它们。
pub(crate) fn sign_page_wants_photo(page: &str) -> bool {
    const PHOTO_MARKERS: [&str; 4] = [
        "newOpenPhoto",
        "photoAreaId",
        "CLIENT_CHOOSE_IMAGE_RESULT",
        "PHOTO_RESULT",
    ];
    PHOTO_MARKERS.iter().any(|marker| page.contains(marker))
}

/// 平台签到响应的成败判定：stuSignajax 成功返回含 success 的文本；
/// 「已签到/签到成功」视为正向（可能手机端已签，命令行补签无害）。
/// 其余（距离不足、参数错误等）按失败处理。
pub(super) fn sign_response_ok(response: &str) -> bool {
    let text = response.to_lowercase();
    text.contains("success") || response.contains("签到成功") || response.contains("已签到")
}

/// 平台说「您已签到过了」：说明**此前已签**（多半是用户自己在手机上签的），
/// 不是本次提交签成的——回执与归因都要分开说，别把用户的功劳记成自动签到的。
pub(super) fn sign_already_done(response: &str) -> bool {
    let text = response.to_lowercase();
    response.contains("已签到") && !response.contains("签到成功") && !text.contains("success")
}

/// 课表窗口向前后各延伸的秒数（覆盖提前签到与拖堂后补签）。
const CLASS_WINDOW_EXTEND_SECS: i64 = 15 * 60;
/// 课表窗口外的兜底实轮询间隔（防课表外的临时签到漏检，同时压住请求量）。
const OFF_WINDOW_POLL_SECS: i64 = 30 * 60;
/// 命中「请勿频繁操作」后窗口外轮询的冷却时长；窗口内不受影响（签到时效优先）。
const RATE_LIMIT_COOLDOWN_SECS: i64 = 2 * 3600;

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
    /// 本轮是否因不在课表窗口而跳过（未发任何网络请求）。
    pub window_skipped: bool,
    /// 活动列表拉取失败的课程数（限流/接口变更时 >0）；>0 说明监控部分或全部失明。
    pub course_errors: usize,
    /// 最近一次活动列表失败的原因（限流恢复自查用）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_course_error: Option<String>,
    pub notified: bool,
}

/// 自动签到单次尝试的结果。
enum AutoSignOutcome {
    /// 平台接受：文案进回执。
    Signed(String),
    /// 平台明确拒绝或参数不全：标记已尝试、文案进回执、不再自动重试。
    /// retry：可重试时附（按钮文案, 命令），手势等只能手动的不给。
    Rejected {
        line: String,
        retry: Option<(String, String)>,
    },
    /// 网络等瞬时错误：不动快照，下轮自然重试。
    Transient,
}

/// 学习通签到监控：协议端登录 → 课程与签到活动枚举 →
/// 新活动落 records 去重 → 待签活动经默认出口提醒。
///
/// 通知策略：
/// - 待签活动（进行中且未签）首次发现 → auto_sign 开启时当场自动签（二维码除外：
///   enc 只在二维码图片里，提醒把照片发给机器人，由 service::qr 消费签入；
///   手势直签会被平台风控拦（90002），client 层自动过滑块拿 validate 重签，
///   仍失败则 ❌ 回执指路手动签——手势窗口常仅约 2 分钟）；
///   关闭自动签 → 提醒一次（含 activity_id）；
/// - 已结束/已签活动只入快照，不提醒；
/// - 登录态失效 → 每日最多提醒一次。
///
/// 自动签策略：平台接受 → ✅ 回执；平台明确拒绝 → 打「已尝试」标记、❌ 回执、
/// 不自动重试（避免对同一活动反复撞）；网络等瞬时错误 → 快照不动，下轮自然重试。
/// 所有签到动作均有 QQ 回执可审计。
#[derive(Clone)]
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
    /// 窗口再按学习通课程名单模糊过滤（jwxt「大学英语I(综合基础)」≈ 学习通
    /// 「大学英语新视野」）：学习通没有这门课时窗口不开，进一步压请求量。
    /// 无缓存名单时回退到全部窗口（冷启动保底）。
    fn in_class_window(&self, now: i64) -> Result<bool> {
        let titles = self.chaoxing_course_titles()?;
        let mut windows = Vec::new();
        for doc in self.store.documents("event", 10_000)? {
            let Ok(item) =
                serde_json::from_value::<ru41::domain::schedule::EventItem>(doc.value.clone())
            else {
                continue;
            };
            if item.status != ru41::domain::schedule::EventStatus::Active {
                continue;
            }
            if item.source.as_ref().map(|s| s.channel.as_str()) != Some("jwxt") {
                continue;
            }
            if !titles.is_empty() && !titles.iter().any(|t| course_title_match(t, &item.title)) {
                continue;
            }
            windows.push((
                item.starts_at - CLASS_WINDOW_EXTEND_SECS,
                item.ends_at + CLASS_WINDOW_EXTEND_SECS,
            ));
        }
        Ok(in_any_window(now, &windows))
    }

    /// 最近一次成功拉取的学习通课程名单（实轮询时刷新）。
    fn chaoxing_course_titles(&self) -> Result<Vec<String>> {
        let titles = self
            .store
            .document("chaoxing_meta", "course_titles")?
            .and_then(|d| {
                d.value
                    .get("titles")
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
            })
            .unwrap_or_default();
        Ok(titles)
    }

    /// 窗口外每 30 分钟允许一轮实轮询（以进入实轮询记账，失败也占用配额）；
    /// 命中平台限流后进入冷却期，冷却未过不允许窗口外轮询，给账号喘息。
    fn off_window_poll_due(&self, now: i64) -> Result<bool> {
        let cooling = self
            .store
            .document("chaoxing_meta", "rate_limited_until")?
            .and_then(|d| d.value.get("ts").and_then(serde_json::Value::as_i64))
            .is_some_and(|ts| now < ts);
        if cooling {
            return Ok(false);
        }
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
                let method = self
                    .resolve_method(&course, &activity, lat, lon, addr, enc, photo_path)
                    .await?;
                let response = self.client.sign(&course, &activity, &method).await?;
                let ok = sign_response_ok(&response);
                let already = sign_already_done(&response);
                self.mark_signed(&course, &activity, if already { "already" } else { "cli" });
                if let Some(notifier) = &self.notifier {
                    let (mark, verb) = if already {
                        ("ℹ️", "此前已签到")
                    } else if ok {
                        ("✅", "签到成功")
                    } else {
                        ("❌", "签到失败")
                    };
                    let message = format!(
                        "{mark} 学习通{}：{}｜{}（平台响应：{}）",
                        verb,
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
    #[allow(clippy::too_many_arguments)]
    async fn resolve_method(
        &self,
        course: &crate::source::ChaoxingCourse,
        activity: &crate::source::ChaoxingActivity,
        lat: Option<f64>,
        lon: Option<f64>,
        addr: Option<&str>,
        enc: Option<&str>,
        photo_path: Option<&str>,
    ) -> Result<crate::source::SignMethod> {
        use crate::source::{ChaoxingSignKind, SignMethod};
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
            // 手势：轨迹作为 signCode 提交。优先级：
            // 平台在窗口内下发的正确轨迹 → 配置里按课程固定的轨迹 → 九键轨迹枚举。
            // 单条轨迹不行时（平台回 90002），由 try_auto_sign 换下一条继续试。
            ChaoxingSignKind::Gesture => {
                let live = self
                    .client
                    .activity_sign_code(course, activity)
                    .await
                    .ok()
                    .flatten();
                let configured = self
                    .config
                    .gesture_sign_codes
                    .get(&course.course_id)
                    .filter(|c| !c.trim().is_empty())
                    .cloned();
                let code = live
                    .or(configured)
                    .or_else(|| super::gesture::candidates().into_iter().next());
                match code {
                    Some(code) => SignMethod::Gesture { code },
                    None => anyhow::bail!("手势签到需要轨迹：平台未下发、配置也没有"),
                }
            }
            // 普通 / 签到码：标题与 sign_type 都看不出是不是「上传图片签到」——
            // 平台把图片签到的活动也标成普通签到（2026-11-06 实测：大学英语新视野
            // 15:30 的「普通签到」，页面里其实是要拍照上传后才签到，objectId 取自
            // 照片）。只有签到页里有照片上传组件才算数，所以这里问页面本身。
            ChaoxingSignKind::Normal | ChaoxingSignKind::Code => {
                if self.page_wants_photo(course, activity).await {
                    let path = match photo_path
                        .map(std::path::PathBuf::from)
                        .or_else(|| self.config.photo_path.clone())
                    {
                        Some(path) => path,
                        None => self.materialize_fallback_photo()?,
                    };
                    SignMethod::Photo { path }
                } else {
                    SignMethod::Normal
                }
            }
        })
    }

    /// 签到页是否要求上传照片（图片签到）：页面里有照片上传组件才算。
    ///
    /// 判据取页面里拍照流程的特征串：`newOpenPhoto`（打开拍照）、`photoAreaId`
    /// （照片区）、`CLIENT_CHOOSE_IMAGE_RESULT`（选图回调）、`PHOTO_RESULT`（提交值）。
    async fn page_wants_photo(
        &self,
        course: &crate::source::ChaoxingCourse,
        activity: &crate::source::ChaoxingActivity,
    ) -> bool {
        let Ok(uid) = self.client.uid() else {
            return false;
        };
        let activity_id = activity.id.to_string();
        let Ok(body) = self.client.pre_sign_body(course, &activity_id, &uid).await else {
            return false;
        };
        sign_page_wants_photo(&body)
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
        std::fs::write(&path, crate::source::fallback_photo_bytes())?;
        Ok(path)
    }

    /// 签到落定后更新快照：标记 user_status=0，poll 侧就不会再播一次「已签」变迁。
    fn mark_signed(
        &self,
        course: &crate::source::ChaoxingCourse,
        activity: &crate::source::ChaoxingActivity,
        via: &str,
    ) {
        let key = format!("{}#{}", course.course_id, activity.id);
        let now = ru41::util::now();
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
        course: &crate::source::ChaoxingCourse,
        activity: &crate::source::ChaoxingActivity,
        note: &str,
    ) {
        let key = format!("{}#{}", course.course_id, activity.id);
        let _ = self.store.document("chaoxing_sign", &key).map(|doc| {
            doc.map(|doc| {
                let mut value = doc.value.clone();
                value["auto_attempted"] = serde_json::json!(ru41::util::now());
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

    /// 签到页链接：PC 端签到页（手势签到可在浏览器里画轨迹）。
    fn sign_page_url(
        &self,
        course: &crate::source::ChaoxingCourse,
        activity: &crate::source::ChaoxingActivity,
    ) -> String {
        format!(
            "https://mobilelearn.chaoxing.com/widget/sign/pcStuSignController/signIn?courseId={}&classId={}&activeId={}",
            course.course_id, course.clazz_id, activity.id
        )
    }

    /// 手势签到：轨迹作为 signCode 提交，错了平台回 90002。
    ///
    /// 候选顺序：平台在窗口内下发的轨迹 → 配置里按课程固定的轨迹 →
    /// 九键规律轨迹枚举（从 1 出发，按"一笔写出来"的顺滑度排序）。
    /// 枚举段不做滑块重试（否则一条轨迹要打三次请求，窗口只有 2 分钟，
    /// 还容易撞上限流）；平台下发的那条照常走滑块流程。
    async fn try_auto_sign_gesture(
        &self,
        course: &crate::source::ChaoxingCourse,
        activity: &crate::source::ChaoxingActivity,
    ) -> AutoSignOutcome {
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(Some(code)) = self.client.activity_sign_code(course, activity).await {
            candidates.push(code);
        }
        if let Some(code) = self
            .config
            .gesture_sign_codes
            .get(&course.course_id)
            .filter(|c| !c.trim().is_empty())
            && !candidates.contains(code)
        {
            candidates.push(code.clone());
        }
        let from_platform = candidates.len();
        for code in super::gesture::candidates() {
            if !candidates.contains(&code) {
                candidates.push(code);
            }
        }
        let started = std::time::Instant::now();
        let mut last = String::new();
        let mut tried = 0usize;
        for (index, code) in candidates
            .into_iter()
            .take(super::gesture::MAX_ATTEMPTS)
            .enumerate()
        {
            // 窗口只有 2 分钟：留 25 秒给人工兜底，枚举不能占满。
            if started.elapsed().as_secs() > 75 {
                break;
            }
            let method = crate::source::SignMethod::Gesture { code: code.clone() };
            let response = match self
                .client
                .sign_with(course, activity, &method, index < from_platform)
                .await
            {
                Ok(response) => response,
                Err(_) => return AutoSignOutcome::Transient,
            };
            tried += 1;
            if sign_already_done(&response) {
                // 有人（多半是用户自己）已经签过，别记成自动签到的功劳。
                self.mark_signed(course, activity, "already");
                return AutoSignOutcome::Signed(format!(
                    "- ℹ️{}｜{}｜此前已签到（平台回：{}），不是本次自动签的",
                    course.title,
                    activity.type_label(),
                    response.trim()
                ));
            }
            if sign_response_ok(&response) {
                self.mark_signed(course, activity, "auto");
                return AutoSignOutcome::Signed(format!(
                    "- ✅{}｜{}｜自动签到成功（轨迹 {}）",
                    course.title,
                    activity.type_label(),
                    code
                ));
            }
            last = response.trim().to_string();
            if last.contains("请勿频繁操作") {
                // 平台限流：立刻停手，别把账号打进去。
                break;
            }
        }
        self.mark_auto_attempted(course, activity, &last);
        AutoSignOutcome::Rejected {
            line: format!(
                "- ❌{}｜{}｜自动签到被平台拒绝（试了 {tried} 条轨迹，响应：{last}）→ 点链接打开学习通手动签：{}",
                course.title,
                activity.type_label(),
                self.sign_page_url(course, activity)
            ),
            retry: None,
        }
    }

    async fn try_auto_sign(
        &self,
        course: &crate::source::ChaoxingCourse,
        activity: &crate::source::ChaoxingActivity,
    ) -> AutoSignOutcome {
        if activity.is_gesture() {
            return self.try_auto_sign_gesture(course, activity).await;
        }
        let method = match self
            .resolve_method(course, activity, None, None, None, None, None)
            .await
        {
            Ok(method) => method,
            Err(err) => {
                self.mark_auto_attempted(course, activity, &err.to_string());
                return AutoSignOutcome::Rejected {
                    line: format!(
                        "- ❌{}｜{}｜自动签到配置不全：{err}",
                        course.title,
                        activity.type_label()
                    ),
                    retry: None,
                };
            }
        };
        match self.client.sign(course, activity, &method).await {
            Ok(response) => {
                if sign_already_done(&response) {
                    self.mark_signed(course, activity, "already");
                    AutoSignOutcome::Signed(format!(
                        "- ℹ️{}｜{}｜此前已签到（平台回：{}），不是本次自动签的",
                        course.title,
                        activity.type_label(),
                        response.trim()
                    ))
                } else if sign_response_ok(&response) {
                    self.mark_signed(course, activity, "auto");
                    AutoSignOutcome::Signed(format!(
                        "- ✅{}｜{}｜自动签到成功（平台响应：{}）",
                        course.title,
                        activity.type_label(),
                        response.trim()
                    ))
                } else {
                    self.mark_auto_attempted(course, activity, response.trim());
                    // 手势的兜底是手机端手画（窗口常仅约 2 分钟），其余类型附重试按钮
                    let (advice, retry) = if activity.is_gesture() {
                        (
                            format!(
                                "→ 手势签到必须带轨迹（signCode）：本次尝试的轨迹平台不认。点链接打开学习通手动签（PC 端可画手势）：{}",
                                self.sign_page_url(course, activity)
                            ),
                            None,
                        )
                    } else {
                        (
                            "→ 点下方按钮重试".to_string(),
                            Some((
                                format!("🔄 重试{}", activity.type_label()),
                                format!("chaoxing sign {}", activity.id),
                            )),
                        )
                    };
                    AutoSignOutcome::Rejected {
                        line: format!(
                            "- ❌{}｜{}｜自动签到被平台拒绝（响应：{}）{advice}",
                            course.title,
                            activity.type_label(),
                            response.trim(),
                        ),
                        retry,
                    }
                }
            }
            // 网络等瞬时错误：快照不动，下一轮 poll 自然重试（窗口常为 1-2 分钟）
            Err(_) => AutoSignOutcome::Transient,
        }
    }

    pub async fn poll_once(&self) -> Result<ChaoxingPollReport> {
        let now = ru41::util::now();
        let today = ru41::util::today_utc(now);

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
                last_course_error: None,
                notified: alerted,
            });
        }

        // 课表窗口自适应轮询（2026-10-29 限流教训：activelist 全天候 60s 轮询
        // 会被平台「请勿频繁操作」掐死，且失败被静默跳过导致整天失明）。
        // 窗口 = 当天课表每节课 前后各 15 分钟；窗口内正常轮询；窗口外完全不发
        // 网络请求，仅每 30 分钟兜底一轮，防课表外的临时签到漏检。
        let in_window = self.in_class_window(now)?;
        if !in_window && !self.off_window_poll_due(now)? {
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
                last_course_error: None,
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
        // 缓存学习通课程名单供课表窗口模糊过滤；空名单不写（异常响应不清空缓存）。
        if !courses.is_empty() {
            self.store.put_document(
                "chaoxing_meta",
                "course_titles",
                None,
                &serde_json::json!({
                    "ts": now,
                    "titles": courses.iter().map(|c| c.title.clone()).collect::<Vec<_>>(),
                }),
            )?;
        }
        let mut sign_activities = 0usize;
        let mut new_activities = 0usize;
        let mut pending = 0usize;
        let mut auto_signed = 0usize;
        let mut signed = 0usize;
        let mut missed = 0usize;
        let mut pending_lines: Vec<String> = Vec::new();
        let mut outcome_lines: Vec<String> = Vec::new();
        // 消息附带的一键按钮：(按钮文案, 回传命令)。QQ 键盘单行最多 4 个。
        let mut pending_buttons: Vec<(String, String)> = Vec::new();
        let mut retry_buttons: Vec<(String, String)> = Vec::new();
        let mut course_errors = 0usize;
        let mut last_course_error: Option<String> = None;
        let mut rate_limited = false;

        for course in &courses {
            let activities = match self.client.activity_list(course).await {
                Ok(activities) => activities,
                Err(err) => {
                    // 单门课程失败只影响本轮该课程，但必须计数 surfaced 到报告：
                    // 全部失败（如平台限流「请勿频繁操作」）时监控等同失明，不能静默。
                    course_errors += 1;
                    let message = format!("{err:#}");
                    if message.contains("请勿频繁操作") {
                        rate_limited = true;
                    }
                    last_course_error = Some(message);
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
                        // 二维码除外（enc 只在二维码图片里）；手势直签虽被平台风控
                        // 拦截（90002），但 client 层会自动过滑块重签，所以照常自动签。
                        if self.config.auto_sign && !activity.is_qr() {
                            match self.try_auto_sign(course, &activity).await {
                                AutoSignOutcome::Signed(line) => {
                                    auto_signed += 1;
                                    outcome_lines.push(line);
                                }
                                AutoSignOutcome::Rejected { line, retry } => {
                                    outcome_lines.push(line);
                                    if let Some(b) = retry {
                                        retry_buttons.push(b);
                                    }
                                }
                                AutoSignOutcome::Transient => {}
                            }
                        } else {
                            if activity.is_qr() {
                                // 科研线 B：窗口期间侦测学生端接口是否泄漏当前 enc
                                self.spawn_enc_probe(activity.id);
                            }
                            if self.notifier.is_some() {
                                pending_lines.push(format!(
                                    "- ⏰{}｜{}｜进行中，截止 {}{}",
                                    course.title,
                                    activity.type_label(),
                                    format_ms(activity.end_time).unwrap_or_else(|| "未知".into()),
                                    if activity.is_qr() {
                                        "｜把二维码照片发给机器人即可代签"
                                    } else {
                                        ""
                                    },
                                ));
                                if activity.is_gesture() {
                                    // 手势签到带 signCode，机器人直签必然 90002
                                    // （公开抓包结论 + 2026-10-30 起实测稳定失败）。
                                    // 给签到页链接，人在浏览器里画手势——PC 端支持。
                                    pending_lines.push(format!(
                                        "- ✍️{}｜{}｜手势签到请点链接自己画：{}",
                                        course.title,
                                        activity.type_label(),
                                        self.sign_page_url(course, &activity)
                                    ));
                                } else if !activity.is_qr() {
                                    pending_buttons.push((
                                        format!("✍️ 去签到（{}）", activity.type_label()),
                                        format!("chaoxing sign {}", activity.id),
                                    ));
                                }
                            }
                        }
                    }
                    continue;
                };

                // 已见过：若自动签开启且仍未签、未被打过「已尝试」标记 → 补试一轮
                // （覆盖上次网络瞬时错误的自然重试；平台已拒绝的不会进来。
                // 二维码缺 enc 是确定性失败，不补试）
                let snapshot_user = doc
                    .value
                    .get("user_status")
                    .and_then(serde_json::Value::as_i64);
                if self.config.auto_sign
                    && activity.is_pending()
                    && !activity.is_qr()
                    && snapshot_user == Some(1)
                    && doc.value.get("auto_attempted").is_none()
                {
                    match self.try_auto_sign(course, &activity).await {
                        AutoSignOutcome::Signed(line) => {
                            auto_signed += 1;
                            outcome_lines.push(line);
                        }
                        AutoSignOutcome::Rejected { line, retry } => {
                            outcome_lines.push(line);
                            if let Some(b) = retry {
                                retry_buttons.push(b);
                            }
                        }
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
                    // activelist 的 userStatus 滞后，用户手动签了它也报未签
                    // （2026-11-05 实测：15:31:33 手动签成功，机器人仍误报漏签）。
                    // 判漏签前用签到页复核，签了就算数，别冤枉人。
                    if self
                        .client
                        .confirm_signed(course, &activity)
                        .await
                        .unwrap_or(false)
                    {
                        signed += 1;
                        let mut fixed = new_value.clone();
                        fixed["user_status"] = serde_json::json!(0);
                        fixed["signed_via"] = serde_json::json!("manual");
                        let _ = self.store.put_document(
                            "chaoxing_sign",
                            &key,
                            Some(doc.revision),
                            &fixed,
                        );
                        outcome_lines.push(format!(
                            "- ✅{}｜{}｜已签到（平台复核：手动签的）",
                            course.title,
                            activity.type_label()
                        ));
                    } else {
                        missed += 1;
                        outcome_lines.push(format!(
                            "- ❌{}｜{}｜已结束，仍未签到",
                            course.title,
                            activity.type_label()
                        ));
                    }
                }
            }
        }

        let mut notified = false;
        // 命中平台限流：窗口外轮询进入冷却期，降低请求压力给账号喘息。
        if rate_limited {
            self.store.put_document(
                "chaoxing_meta",
                "rate_limited_until",
                None,
                &serde_json::json!({ "ts": now + RATE_LIMIT_COOLDOWN_SECS }),
            )?;
        }
        // 全部课程活动列表都拉取失败（登录态却正常）：签到监控处于失明状态，
        // 典型诱因是平台限流（「请勿频繁操作」）或接口变更。
        // 只在课表窗口内告警（失明的代价是错过签到）；窗口外兜底轮询失败
        // 不影响任何课程，记报告不打扰。每日最多告警一次。
        if in_window && !courses.is_empty() && course_errors == courses.len() {
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
                let mut card =
                    ru41::util::msg::Card::new(format!("✍️ 学习通签到（{pending} 个待签）"));
                for line in &pending_lines {
                    card.raw(line);
                }
                card.hint(if pending_buttons.is_empty() {
                    "二维码需现场 enc"
                } else {
                    "点下方按钮一键签到；二维码需现场 enc"
                });
                let message = card.build();
                let mut markup = ru41::ports::commands::ReplyMarkup::new();
                markup.push_row(
                    pending_buttons
                        .iter()
                        .take(4)
                        .map(|(label, cmd)| {
                            ru41::ports::commands::MarkupButton::new(label.clone(), cmd.clone())
                        })
                        .collect(),
                );
                notifier
                    .send_with_markup(&self.config.notify_target, None, &message, &markup)
                    .await?;
                notified = true;
            }
            if !outcome_lines.is_empty() {
                let mut card = ru41::util::msg::Card::new("✅ 学习通签到回执");
                for line in &outcome_lines {
                    card.raw(line);
                }
                let message = card.build();
                let mut markup = ru41::ports::commands::ReplyMarkup::new();
                markup.push_row(
                    retry_buttons
                        .iter()
                        .take(4)
                        .map(|(label, cmd)| {
                            ru41::ports::commands::MarkupButton::new(label.clone(), cmd.clone())
                        })
                        .collect(),
                );
                notifier
                    .send_with_markup(&self.config.notify_target, None, &message, &markup)
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
            last_course_error,
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

impl ru41::ingress::schedule::PollJob for ChaoxingTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        let owned = self.clone();
        ru41::plugins::bridge(async move { Ok(serde_json::to_value(owned.poll_once().await?)?) })
    }
}

/// 纯函数窗口判定（边界含端点），抽出来便于单测。
fn in_any_window(now: i64, windows: &[(i64, i64)]) -> bool {
    windows
        .iter()
        .any(|(start, end)| *start <= now && now <= *end)
}

/// 课表课程名 ⇄ 学习通课程名的模糊匹配：去括号内容与空白后，最长公共前缀
/// ≥ 4 字符（「大学英语I(综合基础)」≈「大学英语新视野」），或一方包含另一方
/// （短课名如「体育」）。刻意保守：误开窗口的代价只是多几轮请求。
fn course_title_match(a: &str, b: &str) -> bool {
    let normalize = |s: &str| -> String {
        s.split(['（', '('])
            .next()
            .unwrap_or(s)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    };
    let (a, b) = (normalize(a), normalize(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a.contains(&b) || b.contains(&a) {
        return true;
    }
    let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    common >= 4
}

#[cfg(test)]
mod tests {
    #[test]
    fn photo_sign_page_is_detected_by_widget_markers() {
        let photo_page = r#"
            <div class="conDiv" onclick="newOpenPhoto();">
            <div class="imgDiv"><img id="image_1" src="https://home-yd.chaoxing.com/res/images/tools/05.gif" /></div>
            <script>var PHOTO_RESULT = ""; var handler = jsBridge_cmd_handlers["CLIENT_CHOOSE_IMAGE_RESULT"];</script>
        "#;
        assert!(super::sign_page_wants_photo(photo_page));
    }

    #[test]
    fn normal_sign_page_is_not_mistaken_for_photo() {
        let normal_page = r#"
            <button onclick="sign();">签到</button>
            <script>function sign(){ var url="/pptSign/stuSignajax?activeId=1"; }</script>
        "#;
        assert!(!super::sign_page_wants_photo(normal_page));
    }

    #[test]
    fn already_signed_is_not_our_success() {
        use super::{sign_already_done, sign_response_ok};
        // 平台说「您已签到过了」：已签状态为真，但不是本次提交签成的。
        assert!(sign_response_ok("您已签到过了"));
        assert!(sign_already_done("您已签到过了"));
        // 真正签成：不算「此前已签」。
        assert!(sign_response_ok("success"));
        assert!(!sign_already_done("success"));
        assert!(sign_response_ok("签到成功"));
        assert!(!sign_already_done("签到成功"));
        // 被拒：两者皆否。
        assert!(!sign_response_ok("签到失败[90002]"));
        assert!(!sign_already_done("签到失败[90002]"));
    }

    use super::{course_title_match, in_any_window};
    use crate::{ChaoxingTracker, config::ChaoxingSection, source::ChaoxingClient};
    use anyhow::Result;
    use ru41::adapters::database::sqlite::SqliteTaskStore;
    use ru41::ports::storage::TaskStore;
    use std::sync::Arc;

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

    #[test]
    fn course_title_fuzzy_match() {
        assert!(course_title_match("大学英语I(综合基础)", "大学英语新视野"));
        assert!(course_title_match("体育", "体育俱乐部"));
        assert!(!course_title_match("高等数学I(理)", "大学英语新视野"));
        assert!(!course_title_match("思想道德与法治", "大学英语新视野"));
        assert!(!course_title_match("心理健康", ""));
    }

    #[test]
    fn off_window_poll_respects_rate_limit_cooldown() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db"))?);
        let section = ChaoxingSection {
            phone: Some("test".into()),
            password: Some("test".into()),
            ..Default::default()
        };
        let config = section.resolved("user:test".into())?;
        let client = ChaoxingClient::new("test", "test", dir.path().join("cookies.json"));
        let tracker = ChaoxingTracker::new(store.clone(), client, None, config);

        let now = 1_000_000;
        // 冷启动允许兜底轮询
        assert!(tracker.off_window_poll_due(now)?);
        store.put_document(
            "chaoxing_meta",
            "last_full_poll",
            None,
            &serde_json::json!({ "ts": now }),
        )?;
        // 30 分钟配额内不允许
        assert!(!tracker.off_window_poll_due(now + 60)?);
        assert!(tracker.off_window_poll_due(now + 1800)?);
        // 命中限流冷却：即使过了 30 分钟也不允许
        store.put_document(
            "chaoxing_meta",
            "rate_limited_until",
            None,
            &serde_json::json!({ "ts": now + 7200 }),
        )?;
        assert!(!tracker.off_window_poll_due(now + 1800)?);
        assert!(!tracker.off_window_poll_due(now + 7199)?);
        // 冷却结束后恢复（同时满足 30 分钟配额）
        assert!(tracker.off_window_poll_due(now + 7201)?);
        Ok(())
    }
}
