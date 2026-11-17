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
            "target_dis_m": info.goal_raw.as_ref().and_then(|g| g.semester_goal),
            "done_count": info.goal_raw.as_ref().and_then(|g| g.semester_valid),
            "days_left": info.goal.as_ref().and_then(|g| g.days_left),
            "records": records.len(),
            "cheat_status": cheat.status,
            "markdown": render::render_all(&info, &records, &cheat),
        }))
    }
}

// ── 规则面 / 预演 / 提交 ─────────────────────────────────────────────

/// 规则面：补跑窗口、必过点、围栏 + **服务端自己的门槛与上次判定**。
///
/// 门槛各门课不一样（实测有「最少1.00公里/跑7分钟以上」，也有「最少1.50公里/
/// 跑6分钟以上」），所以一律从服务端读，插件不写死。
pub async fn fetch_rules(client: &CampusrunClient) -> Result<String, CampusrunError> {
    let start_times = client.start_times().await?;
    let points = client.points().await?;
    let fences = client.fences().await?;
    let verdict = client.last_verdict().await.ok().flatten();
    let mut text = crate::render::render_rules(&start_times, &points, &fences, verdict.as_ref());
    if let Some(verdict) = &verdict {
        text.push_str("\n\n");
        text.push_str(&crate::render::render_verdict(verdict));
    }
    Ok(text)
}

/// 组装一次跑的参数。
///
/// 三个来源分清：
/// - **配置**：配速区间、海拔区间、起跑方式（now/rand/at/ago）；
/// - **服务端**：时长下限（上次记录的 reasonList「跑N分钟以上」）、
///   **起跑时刻**（`/api/starttimes`，控制台 `resolveStartAt()` 走的就是它）；
/// - **用户**：圈数或公里数。
pub async fn prepare_params(
    client: &CampusrunClient,
    section: &CampusrunSection,
    intent: &RunIntent,
) -> Result<RunParams, CampusrunError> {
    let verdict = client.last_verdict().await.ok().flatten();
    let rule_min = verdict.as_ref().and_then(crate::render::rule_min_minutes);
    let now_ms = ru41::util::now() * 1000;
    let base = crate::submit::base_params(section, intent, rule_min, now_ms)
        .map_err(|error| CampusrunError::Invalid(format!("{error:#}")))?;
    let start = fetch_start(client, section, base.duration_min).await?;
    Ok(crate::submit::plan_params(base, start))
}

/// 起跑时刻：**问服务端要**，不自己算。
///
/// 服务端 `/api/starttimes` 会照它的 limits（maxDaysAgo、随机小时窗口）
/// 算出合法起点，必要时还会把时段**收窄**（clamped）——控制台遇到收窄会弹窗
/// 让用户确认，插件只转述。越界（比如往前补 5 天）也是服务端报错。
async fn fetch_start(
    client: &CampusrunClient,
    section: &CampusrunSection,
    duration_min: f64,
) -> Result<crate::submit::StartAt, CampusrunError> {
    let query = crate::submit::start_query(section, duration_min)
        .map_err(|error| CampusrunError::Invalid(format!("{error:#}")))?;
    let Some(query) = query else {
        // now 模式：不发 startAtMs（跟控制台一致）。
        return Ok(crate::submit::StartAt::default());
    };
    let times = client.start_times_query(&query).await?;
    Ok(crate::submit::StartAt {
        at_ms: times.start_ms,
        clamped: times.clamped.unwrap_or(false),
    })
}

/// 预演：换算 → `POST /api/plan`（**不提交**）。
///
/// 结论一律用工具自己的口径（`data.ok` / `error` / `issueSummary`）：
/// 插件不另造一套判定，也不自己写死门槛——距离门槛取服务端 `policy.minDistance`。
pub async fn fetch_plan(
    client: &CampusrunClient,
    section: &CampusrunSection,
    intent: &RunIntent,
) -> Result<(String, PlanData, RunParams), CampusrunError> {
    let params = prepare_params(client, section, intent).await?;
    let request = PlanRequest::campusrun(&params);
    let plan = client.plan(&request).await?;
    // 服务端要求的最小里程（结构化字段优先，失败时从工具自己的策略日志读）。
    let min_distance = crate::render::policy_min_distance(&plan);
    // 工具自己说不行，就照它的话报错——并把它的致命/警告行原样带上。
    if plan.ok == Some(false) {
        let reason = plan
            .error
            .clone()
            .unwrap_or_else(|| plan.issue_summary.clone().unwrap_or_default());
        let mut message = format!("工具预演没通过：{reason}");
        for line in crate::render::tool_verdict_lines(&plan) {
            message.push_str(&format!("\n- {line}"));
        }
        // 服务端那条最小里程要求顺带说清楚（数值还是服务端的）。
        if let Some(min) = min_distance
            && params.distance_km * 1000.0 < min
        {
            message.push_str(&format!(
                "\n（服务端还要求单次至少 {}，现在 {:.2} km）",
                crate::render::fmt_goal_distance(min),
                params.distance_km
            ));
        }
        return Err(CampusrunError::Invalid(message));
    }
    // 距离门槛用服务端当前规则（policy.minDistance），不写死数值。
    if let Some(min) = min_distance
        && params.distance_km * 1000.0 < min
    {
        let hint = match params.laps {
            Some(laps) => {
                let need = (min / crate::submit::lap_length_m(params.lane)).ceil();
                format!(
                    "—— {laps} 圈只有 {:.2} km，最少 {need} 圈",
                    params.distance_km
                )
            }
            None => String::new(),
        };
        return Err(CampusrunError::Invalid(format!(
            "服务端要求单次至少 {}，现在 {:.2} km{hint}",
            crate::render::fmt_goal_distance(min),
            params.distance_km
        )));
    }
    let verdict = client.last_verdict().await.ok().flatten();
    let text = crate::render::render_plan(&plan, &params, verdict.as_ref());
    Ok((text, plan, params))
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
        start_at_ms: params.start_at_ms,
        start_clamped: params.start_clamped,
        alt_range: params.alt_range,
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
///
/// 「上次提交」优先用**服务端记的** `lastSubmitMs`（`/api/accounts`）——
/// 你从 Web 控制台交的那次，插件自己是不知道的，自己记账就会漏掉那次。
/// 服务端读不到时才退回本地那条（兜底，防重复提交）。
///
/// 间隔多长由配置 `silence_secs` 定：服务端的间隔阈值**没有在 API 里暴露**，
/// 所以这里取一个保守值，不是服务端说的数。
pub async fn silence_left(
    store: &Arc<dyn TaskStore>,
    client: &CampusrunClient,
    section: &CampusrunSection,
    now: i64,
) -> Result<i64, CampusrunError> {
    let local = store
        .document(crate::submit::SUBMIT_KIND, crate::submit::LAST_SUBMIT_ID)
        .ok()
        .flatten()
        .and_then(|doc| serde_json::from_value::<LastSubmit>(doc.value).ok())
        .map(|last| last.last_submit_at);
    let server = client.accounts().await.ok().and_then(|data| {
        let current = data
            .accounts
            .iter()
            .find(|item| item.current.unwrap_or(false))
            .or_else(|| {
                data.accounts
                    .iter()
                    .find(|item| Some(item.id.as_deref().unwrap_or_default()) == data.current.as_deref())
            })?;
        item_last_submit_ms(current)
    });
    let last = match (server, local) {
        (Some(server), Some(local)) => Some(server.max(local)),
        (Some(server), None) => Some(server),
        (None, local) => local,
    };
    Ok(match last {
        Some(last) => (section.silence_secs() - (now - last)).max(0),
        None => 0,
    })
}

/// 服务端记的上次提交时刻（毫秒 → 秒）。
fn item_last_submit_ms(account: &crate::model::AccountSummary) -> Option<i64> {
    account.last_submit_ms.map(|ms| ms / 1000)
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
    let left = silence_left(store, client, section, now).await?;
    if left > 0 {
        anyhow::bail!(
            "上一次提交（服务端记录的）后 {silence} 秒内不再发跑步请求，还剩 {left} 秒",
            silence = section.silence_secs()
        );
    }

    pending.state = "submitting".to_owned();
    save_pending(store, &pending)?;

    let request = PlanRequest::campusrun(&crate::submit::RunParams {
        distance_km: pending.distance_km,
        duration_min: pending.duration_min,
        lane: pending.lane,
        laps: pending.laps,
        start_at_ms: pending.start_at_ms,
        start_clamped: pending.start_clamped,
        alt_range: pending.alt_range,
    });
    match client.run(&request).await {
        Ok(run) => {
            // 工具返回 submitted=false 就是没提交成（自检没过），照它的话报。
            if run.submitted == Some(false) {
                pending.state = "failed".to_owned();
                pending.error = Some("工具自检没过，没有提交".to_owned());
                save_pending(store, &pending)?;
                anyhow::bail!("提交没成功：工具自检没过（详见预演日志）");
            }
            let rrid = run.record_id;
            pending.state = "accepted".to_owned();
            pending.rrid = rrid;
            pending.error = None;
            save_pending(store, &pending)?;
            // 本地留一条**兜底**记录：服务端读不到 lastSubmitMs 时用得上。
            // 权威值在服务端（/api/accounts），这里不另造台账语义。
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
