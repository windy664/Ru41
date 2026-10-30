//! 微助教签到监控：轮询待签列表 → 新活动提醒/自动签 → 结束收敛回执。
//!
//! 与学习通的关键差异（见 docs/teachermate-recon.md）：
//! - 平台无 user_status 字段，是否已签只能由 sign_in 响应得知；
//! - 凭证是人工投喂的 openid，无账密重登，失效走每日一次告警；
//! - 二维码签到的本质是"用微信身份打开轮换 URL"（chunk 39 反编译）；
//!   v1 不做替扫（缺真实 qrUrl 样本验证 openid 是否被认），只提醒现场扫，
//!   faye 抓码验证后可升级为全自动（见 recon 实测记录 3）。

pub mod work;

pub use work::{TeachermateWorkPollReport, TeachermateWorkTracker};

use super::config::TeachermateConfig;
use super::source::{ActiveSign, SignsOutcome, TeachermateClient, sign_response_ok};
use crate::ports::{channel::NotifyChannel, storage::TaskStore};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;

/// 一轮轮询的报告。
#[derive(Debug, Serialize)]
pub struct TeachermatePollReport {
    /// openid 是否有效（false 时本轮只做告警）。
    pub openid_ok: bool,
    /// 平台返回的进行中签到数。
    pub active_signs: usize,
    /// 本轮首次发现的签到数。
    pub new_signs: usize,
    /// 本轮自动签到成功的活动数（auto_sign 开启时）。
    pub auto_signed: usize,
    /// 本轮检测到「已从进行中列表消失且本侧无签到记录」的活动数。
    pub ended_unsigned: usize,
    pub notified: bool,
}

/// 自动签到单次尝试的结果（语义同 chaoxing：拒绝不重试，瞬时错误下轮重试）。
enum AutoSignOutcome {
    Signed(String),
    Rejected(String),
    Transient,
}

/// 微助教签到监控：openid 直连待签列表 → 新活动落 records 去重 →
/// auto_sign 开启时当场自动签（二维码除外）→ 消失的活动给结束回执。
///
/// 通知策略：
/// - 待签活动首次发现 → 自动签或提醒一次（消息带 sign_id，可 `teachermate sign`）；
/// - 结束收敛：从进行中列表消失且本侧无 signed_at → 🏁 回执一次
///   （平台不暴露已签状态，已手签的用户可忽略该回执）；
/// - openid 失效 → 每日最多告警一次，指路 bind 命令。
pub struct TeachermateTracker {
    store: Arc<dyn TaskStore>,
    notifier: Option<Arc<dyn NotifyChannel>>,
    client: TeachermateClient,
    config: TeachermateConfig,
}

const RECORD_KIND: &str = "teachermate_sign";
const ALERT_KIND: &str = "teachermate_alert";

impl TeachermateTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        client: TeachermateClient,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: TeachermateConfig,
    ) -> Self {
        Self {
            store,
            notifier,
            client,
            config,
        }
    }

    /// 手动签到：按 sign_id 在进行中列表找到活动后发起签到。
    /// GPS 活动坐标取命令行 > 配置缺省；二维码活动无法代签（提示现场扫）。
    /// 结果回执经通知出口推送，同时把快照标记已签，避免 poll 再播结束回执。
    pub async fn sign_manual(
        &self,
        sign_id: i64,
        lat: Option<f64>,
        lon: Option<f64>,
    ) -> Result<String> {
        let signs = match self.client.active_signs().await? {
            SignsOutcome::Active(signs) => signs,
            SignsOutcome::Expired => anyhow::bail!(
                "微助教 openid 已失效：微信服务号里点任意菜单，复制链接后 `teachermate bind <链接>` 重新投喂"
            ),
        };
        let sign = signs
            .into_iter()
            .find(|sign| sign.sign_id == sign_id)
            .with_context(|| format!("找不到进行中的签到 {sign_id}（可能已结束）"))?;
        if sign.is_qr() {
            anyhow::bail!("二维码签到无法代签：请在微信里打开微助教现场扫码");
        }
        let location = if sign.is_gps() {
            match (lat, lon) {
                (Some(lat), Some(lon)) => Some((lat, lon)),
                _ => self.config.default_location,
            }
        } else {
            None
        };
        let response = self.client.sign_in(&sign, location).await?;
        let ok = sign_response_ok(&response);
        self.mark_signed(&sign, "cli");
        if let Some(notifier) = &self.notifier {
            let mark = if ok { "✅" } else { "❌" };
            let message = format!(
                "{mark} 微助教{}：{}｜id={}（平台响应：{}）",
                if ok { "签到成功" } else { "签到失败" },
                sign.name,
                sign.sign_id,
                response.trim()
            );
            notifier
                .send(&self.config.notify_target, None, &message)
                .await?;
        }
        Ok(response)
    }

    /// 自动签到一次：成功/拒绝给回执文案，瞬时错误静默（下轮自然重试）。
    async fn try_auto_sign(&self, sign: &ActiveSign) -> AutoSignOutcome {
        // GPS 活动带缺省坐标（若有）；服务端校验弱，无坐标也发普通载荷
        let location = if sign.is_gps() {
            self.config.default_location
        } else {
            None
        };
        match self.client.sign_in(sign, location).await {
            Ok(response) => {
                if sign_response_ok(&response) {
                    self.mark_signed(sign, "auto");
                    AutoSignOutcome::Signed(format!(
                        "- ✅{}｜{}｜自动签到成功（平台响应：{}）",
                        sign.name,
                        sign.kind_label(),
                        response.trim()
                    ))
                } else {
                    self.mark_rejected(sign, response.trim());
                    AutoSignOutcome::Rejected(format!(
                        "- ❌{}｜{}｜自动签到被平台拒绝（响应：{}）→ 可 `teachermate sign {}` 重试",
                        sign.name,
                        sign.kind_label(),
                        response.trim(),
                        sign.sign_id
                    ))
                }
            }
            // 网络等瞬时错误：快照不动，下一轮 poll 自然重试（窗口常为 1-2 分钟）
            Err(_) => AutoSignOutcome::Transient,
        }
    }

    fn snapshot_key(sign: &ActiveSign) -> String {
        format!("{}#{}", sign.course_id, sign.sign_id)
    }

    /// 签到落定后更新快照：标记 signed_at，poll 侧不再播该活动的结束回执。
    /// 快照不存在时（如直接手动签、poll 还没见过）现场补一条最小快照。
    fn mark_signed(&self, sign: &ActiveSign, via: &str) {
        let key = Self::snapshot_key(sign);
        let now = crate::util::now();
        let result = match self.store.document(RECORD_KIND, &key) {
            Ok(Some(doc)) => {
                let mut value = doc.value.clone();
                value["signed_at"] = serde_json::json!(now);
                value["signed_via"] = serde_json::json!(via);
                self.store
                    .put_document(RECORD_KIND, &key, Some(doc.revision), &value)
            }
            Ok(None) => self.store.put_document(
                RECORD_KIND,
                &key,
                None,
                &serde_json::json!({
                    "course": sign.name,
                    "kind": sign.kind_label(),
                    "course_id": sign.course_id,
                    "sign_id": sign.sign_id,
                    "is_gps": sign.is_gps,
                    "is_qr": sign.is_qr,
                    "first_seen": now,
                    "signed_at": now,
                    "signed_via": via,
                }),
            ),
            Err(err) => Err(err),
        };
        // 快照更新失败只影响去重精度（poll 可能补一条结束回执），不掩盖签到结果
        let _ = result;
    }

    /// 平台明确拒绝后打「已尝试」标记：自动签不会对同一活动反复重试。
    fn mark_rejected(&self, sign: &ActiveSign, note: &str) {
        let key = Self::snapshot_key(sign);
        let _ = self.store.document(RECORD_KIND, &key).map(|doc| {
            doc.map(|doc| {
                let mut value = doc.value.clone();
                value["auto_note"] = serde_json::json!(note);
                value["auto_note_at"] = serde_json::json!(crate::util::now());
                self.store
                    .put_document(RECORD_KIND, &key, Some(doc.revision), &value)
            })
        });
    }

    fn pending_line(&self, sign: &ActiveSign) -> String {
        if sign.is_qr() {
            format!(
                "- ⏰{}｜二维码签到｜进行中｜请在微信里打开微助教现场扫码（二维码轮换，无法代签）",
                sign.name
            )
        } else {
            format!(
                "- ⏰{}｜{}｜进行中｜id={}｜`teachermate sign {}`",
                sign.name,
                sign.kind_label(),
                sign.sign_id,
                sign.sign_id
            )
        }
    }

    pub async fn poll_once(&self) -> Result<TeachermatePollReport> {
        let now = crate::util::now();
        let today = crate::util::today_utc(now);

        let signs = match self.client.active_signs().await? {
            SignsOutcome::Active(signs) => signs,
            SignsOutcome::Expired => {
                // openid 失效：每日最多告警一次，指路 bind 命令
                let key = format!("openid:{today}");
                let nagged = self.store.document(ALERT_KIND, &key)?.is_some();
                let mut alerted = false;
                if !nagged && let Some(notifier) = self.notifier.as_ref() {
                    self.store.put_document(
                        ALERT_KIND,
                        &key,
                        None,
                        &serde_json::json!({ "ts": now }),
                    )?;
                    notifier
                        .send(
                            &self.config.notify_target,
                            None,
                            "⚠️ 微助教 openid 已失效：微信服务号里点任意菜单，复制链接后 `teachermate bind <链接>` 重新投喂",
                        )
                        .await?;
                    alerted = true;
                }
                return Ok(TeachermatePollReport {
                    openid_ok: false,
                    active_signs: 0,
                    new_signs: 0,
                    auto_signed: 0,
                    ended_unsigned: 0,
                    notified: alerted,
                });
            }
        };

        let mut new_signs = 0usize;
        let mut auto_signed = 0usize;
        let mut ended_unsigned = 0usize;
        let mut pending_lines: Vec<String> = Vec::new();
        let mut outcome_lines: Vec<String> = Vec::new();
        let mut active_keys: HashSet<String> = HashSet::new();

        for sign in &signs {
            let key = Self::snapshot_key(sign);
            active_keys.insert(key.clone());
            let existing = self.store.document(RECORD_KIND, &key)?;
            let Some(doc) = existing else {
                // 首次发现：落快照 → 自动签或提醒
                self.store.put_document(
                    RECORD_KIND,
                    &key,
                    None,
                    &serde_json::json!({
                        "course": sign.name,
                        "kind": sign.kind_label(),
                        "course_id": sign.course_id,
                        "sign_id": sign.sign_id,
                        "is_gps": sign.is_gps,
                        "is_qr": sign.is_qr,
                        "first_seen": now,
                    }),
                )?;
                new_signs += 1;
                if self.config.auto_sign && !sign.is_qr() {
                    match self.try_auto_sign(sign).await {
                        AutoSignOutcome::Signed(line) => {
                            auto_signed += 1;
                            outcome_lines.push(line);
                        }
                        AutoSignOutcome::Rejected(line) => outcome_lines.push(line),
                        AutoSignOutcome::Transient => {}
                    }
                } else {
                    pending_lines.push(self.pending_line(sign));
                }
                continue;
            };

            // 已见过：自动签开启且从未签成、未被平台拒绝 → 补试一轮
            //（覆盖上次瞬时错误的自然重试；拒绝过的带 auto_note 不会进来）
            if self.config.auto_sign
                && !sign.is_qr()
                && doc.value.get("signed_at").is_none()
                && doc.value.get("auto_note").is_none()
            {
                match self.try_auto_sign(sign).await {
                    AutoSignOutcome::Signed(line) => {
                        auto_signed += 1;
                        outcome_lines.push(line);
                    }
                    AutoSignOutcome::Rejected(line) => outcome_lines.push(line),
                    AutoSignOutcome::Transient => {}
                }
            }
        }

        // 结束收敛：从进行中列表消失、本侧又没有签到记录的活动 → 🏁 回执一次。
        // 平台不暴露「是否已签」，已在微信手签的用户按回执文案忽略即可。
        for doc in self.store.documents(RECORD_KIND, 200)? {
            if doc.value.get("ended_at").is_some() || doc.value.get("signed_at").is_some() {
                continue;
            }
            if active_keys.contains(&doc.id) {
                continue;
            }
            let course = doc
                .value
                .get("course")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("未知课程")
                .to_owned();
            let kind = doc
                .value
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("签到")
                .to_owned();
            let mut value = doc.value.clone();
            value["ended_at"] = serde_json::json!(now);
            let updated =
                self.store
                    .put_document(RECORD_KIND, &doc.id, Some(doc.revision), &value)?;
            if updated {
                ended_unsigned += 1;
                outcome_lines.push(format!(
                    "- 🏁{course}｜{kind}｜已结束，本侧未记录到签到（已在微信手签可忽略）"
                ));
            }
        }

        let mut notified = false;
        if let Some(notifier) = &self.notifier {
            if !pending_lines.is_empty() {
                let message = format!(
                    "## ✍️ 微助教签到（{} 个进行中）\n\n{}\n\n> openid 失效时用 `teachermate bind <链接>` 重新投喂）",
                    signs.len(),
                    pending_lines.join("\n")
                );
                notifier
                    .send(&self.config.notify_target, None, &message)
                    .await?;
                notified = true;
            }
            if !outcome_lines.is_empty() {
                let message = format!("## ✅ 微助教签到回执\n\n{}", outcome_lines.join("\n"));
                notifier
                    .send(&self.config.notify_target, None, &message)
                    .await?;
                notified = true;
            }
        }

        Ok(TeachermatePollReport {
            openid_ok: true,
            active_signs: signs.len(),
            new_signs,
            auto_signed,
            ended_unsigned,
            notified,
        })
    }
}

impl crate::ingress::schedule::PollJob for TeachermateTracker {
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
