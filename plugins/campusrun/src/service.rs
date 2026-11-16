//! 校园跑的只读业务逻辑：把控制台数据取回来并渲染成 Markdown。
//!
//! 这里**没有任何写操作**：不提交跑步记录、不碰 OBS、不改账号。
//! 提交类动作留到以后单独评估（风控、时段、静默窗口都要人确认）。

use crate::client::{CampusrunClient, CampusrunError};
use crate::render;

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
