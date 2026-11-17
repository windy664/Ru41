//! 校园跑的业务逻辑：把控制台数据取回来并渲染成 Markdown；
//! 以及「跑一次」的换算 → 门槛 → 预演 → 确认 → 提交这条链。
//!
//! **提交是不可逆的写操作**（跑上去就计入体育成绩，多交还会被风控盯上），
//! 所以默认关着（`allow_submit=false`），且必须经过用户明确确认；
//! 本文件里的测试与自检**不会**真提交。

use crate::client::{CampusrunClient, CampusrunError, PlanRequest};
use crate::config::CampusrunSection;
use crate::model::PlanData;
use crate::render;
use crate::submit::{LastSubmit, PendingSubmit, RunIntent, RunParams};
use ru41::ports::storage::TaskStore;
use std::sync::Arc;

/// 学期进度。
pub async fn fetch_status(client: &CampusrunClient) -> Result<String, CampusrunError> {
    let info = client.semester().await?;
    Ok(render::render_status(&info))
}

/// 最近记录。
pub async fn fetch_records(
    client: &CampusrunClient,
    limit: usize,
) -> Result<String, CampusrunError> {
    let records = client.records().await?;
    Ok(render::render_records(&records, limit))
}

/// 违规名单/风控状态。
pub async fn fetch_cheat(client: &CampusrunClient) -> Result<String, CampusrunError> {
    let cheat = client.cheat().await?;
    Ok(render::render_cheat(&cheat))
}

/// 一页看完：进度 + 最近记录 + 账号状态。
pub async fn fetch_all(client: &CampusrunClient) -> Result<String, CampusrunError> {
    let info = client.semester().await?;
    let records = client.records().await?;
    let cheat = client.cheat().await?;
    Ok(render::render_all(&info, &records, &cheat))
}

/// 按名字分发（CLI 与 QQ 命令共用）。
pub async fn fetch_markdown(
    client: &CampusrunClient,
    what: &str,
    limit: usize,
) -> Result<String, CampusrunError> {
    match what {
        "status" => fetch_status(client).await,
        "records" => fetch_records(client, limit).await,
        "cheat" => fetch_cheat(client).await,
        _ => fetch_all(client).await,
    }
}

/// 只读快照任务：每小时拉一次学期进度与最近记录。
///
/// P0 只把结果落到 `schedule run campusrun` 的输出与容器日志里；
/// 每日推送（QQ 主动提醒）在下一期接入。
#[derive(Clone)]
pub struct CampusrunTracker {
    client: CampusrunClient,
}

impl CampusrunTracker {
    pub fn new(client: CampusrunClient) -> Self {
        Self { client }
    }
}

impl ru41::ingress::schedule::SimplePollJob for CampusrunTracker {
    type Report = serde_json::Value;

    fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(3600)
    }

    async fn poll(&self) -> anyhow::Result<Self::Report> {
        let client = self.client.clone();
        let info = client
            .semester()
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let records = client
            .records()
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let cheat = client
            .cheat()
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let km = |meters: Option<f64>| meters.map(|m| (m / 1000.0 * 100.0).round() / 100.0);
        Ok(serde_json::json!({
            "semester": info.summary_raw.sname,
            "valid_km": km(info.summary_raw.semester_valid_dis),
            "total_km": km(info.summary_raw.semester_dis),
            "target_count": info.goal_raw.as_ref().and_then(|g| g.semester_goal),
            "done_count": info.goal_raw.as_ref().and_then(|g| g.semester_valid),
            "days_left": info.goal.as_ref().and_then(|g| g.days_left),
            "records": records.len(),
            "cheat_status": cheat.status,
            "markdown": render::render_all(&info, &records, &cheat),
        }))
    }
}

// ── 规则面 / 预演 / 提交 ─────────────────────────────────────────────

/// 规则面：补跑窗口、必过点、围栏、单次门槛。
pub async fn fetch_rules(
    client: &CampusrunClient,
    section: &CampusrunSection,
) -> Result<String, CampusrunError> {
    let start_times = client.start_times().await?;
    let points = client.points().await?;
    let fences = client.fences().await?;
    Ok(crate::render::render_rules(
        &start_times,
        &points,
        &fences,
        section.min_distance_m(),
        section.min_duration_sec(),
    ))
}

/// 预演：换算 → 门槛校验 → `POST /api/plan`（**不提交**）。
pub async fn fetch_plan(
    client: &CampusrunClient,
    section: &CampusrunSection,
    intent: &RunIntent,
) -> Result<(String, PlanData), CampusrunError> {
    let params = crate::submit::plan_params(section, intent)
        .map_err(|error| CampusrunError::Invalid(format!("{error:#}")))?;
    let request = PlanRequest::campusrun(params.distance_km, params.duration_min);
    let plan = client.plan(&request).await?;
    let text = crate::render::render_plan(&plan, params.distance_km, params.duration_min);
    Ok((text, plan))
}

/// 登记一条待确认的提交（**只登记，不提交**）。
pub fn register_pending(
    store: &Arc<dyn TaskStore>,
    params: &RunParams,
    plan_summary: &str,
    token: &str,
    now: i64,
) -> anyhow::Result<()> {
    let pending = PendingSubmit {
        token: token.to_owned(),
        distance_km: params.distance_km,
        duration_min: params.duration_min,
        lane: params.lane,
        laps: params.laps,
        state: "planned".to_owned(),
        created_at: now,
        rrid: None,
        error: None,
        plan_summary: plan_summary.to_owned(),
    };
    store.put_document(
        crate::submit::SUBMIT_KIND,
        token,
        None,
        &serde_json::to_value(&pending)?,
    )?;
    Ok(())
}

/// 静默窗口还剩多少秒（0 = 不在窗口里）。
pub fn silence_left(
    store: &Arc<dyn TaskStore>,
    section: &CampusrunSection,
    now: i64,
) -> anyhow::Result<i64> {
    let last = store
        .document(crate::submit::SUBMIT_KIND, crate::submit::LAST_SUBMIT_ID)?
        .and_then(|doc| serde_json::from_value::<LastSubmit>(doc.value).ok());
    Ok(match last {
        Some(last) => (section.silence_secs() - (now - last.last_submit_at)).max(0),
        None => 0,
    })
}

/// 确认并提交一条记录（**写操作！**）。
///
/// 调用前必须已经过三道闸：`allow_submit` 开关、待确认未过期、静默窗口。
/// 结果三态：
/// - `accepted`：拿到 rrid；
/// - `failed`：明确失败（参数/自检/服务端错误）；
/// - `unknown`：**结果未知**（网络断了、服务端可能已入账）——保持不可重试，
///   同一时间段跑两次物理上不可能、最容易被判异常，只能去运动记录页核对。
pub async fn confirm_and_submit(
    store: &Arc<dyn TaskStore>,
    client: &CampusrunClient,
    section: &CampusrunSection,
    token: &str,
    now: i64,
) -> anyhow::Result<SubmitOutcome> {
    let doc = store
        .document(crate::submit::SUBMIT_KIND, token)?
        .ok_or_else(|| anyhow::anyhow!("没有这条待确认的提交（口令 {token} 不存在）"))?;
    let mut pending: PendingSubmit = serde_json::from_value(doc.value)?;
    if pending.state != "planned" {
        anyhow::bail!("这条提交已经是「{}」状态，不能重复提交", pending.state);
    }
    if now - pending.created_at > section.confirm_ttl_secs() {
        pending.state = "expired".to_owned();
        save_pending(store, &pending)?;
        anyhow::bail!(
            "待确认已过期（有效期 {} 分钟），要重新发起一次",
            section.confirm_ttl_secs() / 60
        );
    }
    if !section.allow_submit() {
        anyhow::bail!("提交功能没开：[sources.campusrun] 里 allow_submit = true 才会真提交");
    }
    let left = silence_left(store, section, now)?;
    if left > 0 {
        anyhow::bail!(
            "上一次提交后 {silence} 秒内不能再发跑步请求，还剩 {left} 秒",
            silence = section.silence_secs()
        );
    }

    pending.state = "submitting".to_owned();
    save_pending(store, &pending)?;

    let request = PlanRequest::campusrun(pending.distance_km, pending.duration_min);
    match client.run(&request).await {
        Ok(run) => {
            if run.blocked == Some(true) {
                pending.state = "failed".to_owned();
                pending.error = Some(
                    run.issue_summary
                        .clone()
                        .unwrap_or_else(|| "服务端自检拦下".to_owned()),
                );
                save_pending(store, &pending)?;
                anyhow::bail!(
                    "提交被服务端自检拦下：{}",
                    pending.error.as_deref().unwrap_or("")
                );
            }
            let rrid = run.record_id.or(run.rrid);
            pending.state = "accepted".to_owned();
            pending.rrid = rrid;
            pending.error = None;
            save_pending(store, &pending)?;
            store.put_document(
                crate::submit::SUBMIT_KIND,
                crate::submit::LAST_SUBMIT_ID,
                None,
                &serde_json::to_value(LastSubmit {
                    last_submit_at: now,
                    rrid,
                })?,
            )?;
            Ok(SubmitOutcome::Accepted { rrid })
        }
        Err(error) => {
            // 网络类错误 = 结果未知：服务端可能已经入账，不能自动重试。
            let unknown = matches!(error, CampusrunError::Unreachable { .. });
            pending.state = if unknown { "unknown" } else { "failed" }.to_owned();
            pending.error = Some(error.to_string());
            save_pending(store, &pending)?;
            if unknown {
                Ok(SubmitOutcome::Unknown {
                    reason: pending.error.clone().unwrap_or_default(),
                })
            } else {
                Ok(SubmitOutcome::Failed {
                    reason: pending.error.clone().unwrap_or_default(),
                })
            }
        }
    }
}

/// 提交结果。
#[derive(Debug, Clone)]
pub enum SubmitOutcome {
    /// 成功，拿到记录 id。
    Accepted { rrid: Option<i64> },
    /// 明确失败。
    Failed { reason: String },
    /// 结果未知（别再提交，去运动记录页核对）。
    Unknown { reason: String },
}

impl SubmitOutcome {
    /// 给人看的结论。
    pub fn text(&self) -> String {
        match self {
            Self::Accepted { rrid: Some(rrid) } => {
                format!("✅ 校园跑已提交，记录 id={rrid}")
            }
            Self::Accepted { rrid: None } => "✅ 校园跑已提交（服务端没回记录 id）".to_owned(),
            Self::Failed { reason } => format!("❌ 校园跑提交失败：{reason}"),
            Self::Unknown { reason } => format!(
                "⚠️ 校园跑提交**结果未知**：{reason}\n\n\
                 同一段时间不可能跑两次，先去运动记录页核对，别重发。"
            ),
        }
    }
}

/// 覆盖保存待确认记录（CAS：带上当前 revision）。
///
/// ⚠️ 不能用 `expected=None`：那是 insert-or-ignore，**根本不会覆盖已有文档**——
/// 状态会永远停在 "planned"，同一个口令就能重复提交（2026-11-17 测试抓到）。
fn save_pending(store: &Arc<dyn TaskStore>, pending: &PendingSubmit) -> anyhow::Result<()> {
    let revision = store
        .document(crate::submit::SUBMIT_KIND, &pending.token)?
        .map(|doc| doc.revision);
    let written = store.put_document(
        crate::submit::SUBMIT_KIND,
        &pending.token,
        revision,
        &serde_json::to_value(pending)?,
    )?;
    anyhow::ensure!(
        written,
        "campusrun.submit {} 状态更新竞争失败",
        pending.token
    );
    Ok(())
}
