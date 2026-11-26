//! 事后复查：**提交成功 ≠ 安全**。
//!
//! ## 为什么需要
//!
//! 服务端的跑批判定是**事后**做的（上游 issue #51 实测：提交时六项规则全绿的
//! 记录，4~7 天后仍被判 `isCheat=1`、`appealStatus=4`、申诉通道关闭）。
//! 上游的 `/api/cheat` 只能告诉你「现在被标了几次」，**告诉不了你哪一条、
//! 什么时候被标的** —— 每条记录详情里的 `reasonList` 才有「运动数据有问题」。
//!
//! 所以这里在**每次提交后第 N 天**（默认 3 天）自动把那一条拉出来看：
//! 被判违规就主动推一条消息给你，别等你自己想起来。
//!
//! ## 边界（插件只转述，不编因果）
//!
//! - 只说服务端说了什么（判违规依据、申诉状态、能不能申诉）；
//! - **不**解释「为什么服务端觉得有问题」—— 那是服务端的事，我们只能转述；
//! - 复查结果只推一次（`campusrun.review/<rrid>` 文档做幂等）。

use std::sync::Arc;

use anyhow::Result;
use ru41::ports::channel::NotifyChannel;
use ru41::ports::storage::TaskStore;

use crate::client::CampusrunClient;
use crate::model::{RunRecordDetail, ViolationDetail};
use crate::submit::{LAST_SUBMIT_ID, LastSubmit, SUBMIT_KIND};

/// 复查结果的文档 kind（键 = rrid）。
pub const REVIEW_KIND: &str = "campusrun.review";

/// 提交后过多久复查（秒）。默认 3 天 —— 上游实测违规出现在第 4~7 天，
/// 3 天时第一次查可能还没判，**查了也会记下来**，所以这里留了余量：
/// 真被回扫了，第 4 天那次复查会补上（见 `review_last_submit` 的说明）。
pub const REVIEW_AFTER_SECS: i64 = 3 * 86_400;

/// 一次复查的结果。
#[derive(Debug, Clone)]
pub struct ReviewOutcome {
    pub rrid: i64,
    /// 服务端是否把这条判成了违规/可疑。
    pub flagged: bool,
    /// 判违规的依据（实测就一句「运动数据有问题」）。
    pub cheat_reasons: Vec<String>,
    /// 还能不能再申诉（0 = 不能）。
    pub can_appeal: Option<i64>,
    /// 申诉说明。
    pub appeal_info: Option<String>,
    /// 服务端结论。
    pub status_info: Option<String>,
    /// 没达标但**不算违规**的项（距离/时长这类门槛）。
    pub incomplete_reasons: Vec<String>,
    /// 这条记录的距离（公里）。
    pub distance_km: Option<f64>,
    /// 跑的时候（毫秒）。
    pub start_time: Option<i64>,
}

/// 复查上一次提交的那条记录；被判违规就推一条消息。
///
/// 返回 `Ok(None)` 表示「这次没什么要说的」（没提交过 / 还没到复查时间 /
/// 已经查过 / 查了但没被判违规 —— 没被判违规也要**记一笔**，免得天天重查）。
pub async fn review_last_submit(
    client: &CampusrunClient,
    store: &dyn TaskStore,
    notifier: Option<&Arc<dyn NotifyChannel>>,
    target: &str,
    now_ms: i64,
) -> Result<Option<ReviewOutcome>> {
    let Some(doc) = store.document(SUBMIT_KIND, LAST_SUBMIT_ID)? else {
        return Ok(None);
    };
    let last: LastSubmit = serde_json::from_value(doc.value)?;
    let Some(rrid) = last.rrid else {
        return Ok(None);
    };
    let submitted_at_ms = last.last_submit_at * 1000;
    if now_ms - submitted_at_ms < REVIEW_AFTER_SECS * 1000 {
        return Ok(None); // 还没到复查时间
    }
    let review_id = rrid.to_string();
    if store.document(REVIEW_KIND, &review_id)?.is_some() {
        return Ok(None); // 已经查过并记过账
    }

    let detail: RunRecordDetail = match client.record(rrid).await {
        Ok(detail) => detail,
        Err(error) => {
            // 查不到就别记账（下次还能再查）—— 但也别刷屏。
            eprintln!("[campusrun] 复查 {rrid} 失败：{error}");
            return Ok(None);
        }
    };
    let violation = ViolationDetail::from_detail(&detail);
    let outcome = ReviewOutcome {
        rrid,
        flagged: detail.is_flagged(),
        cheat_reasons: violation.cheat_reasons.clone(),
        can_appeal: violation.can_appeal,
        appeal_info: violation.appeal_info.clone(),
        status_info: violation.status_info.clone(),
        incomplete_reasons: violation.incomplete_reasons.clone(),
        distance_km: violation.distance_km,
        start_time: violation.start_time,
    };
    // 先落账（幂等）：同一个 rrid 只推一次。
    store.put_document(
        REVIEW_KIND,
        &review_id,
        None,
        &serde_json::json!({
            "checked_at": now_ms,
            "flagged": outcome.flagged,
            "cheat_reasons": outcome.cheat_reasons,
            "can_appeal": outcome.can_appeal,
            "rrid": rrid,
        }),
    )?;

    if outcome.flagged
        && let Some(notifier) = notifier
        && !target.trim().is_empty()
    {
        let text = render_review_notice(&outcome);
        if let Err(error) = notifier.send(target, None, &text).await {
            eprintln!("[campusrun] 复查通知发送失败：{error:#}");
        }
    }
    Ok(Some(outcome))
}

/// 复查通知的正文（只转述服务端给的，不编因果）。
fn render_review_notice(outcome: &ReviewOutcome) -> String {
    let mut lines = vec![
        "⚠️ 校园跑事后复查：上一次提交被服务端判违规".to_owned(),
        String::new(),
        format!("- 记录 id：{}", outcome.rrid),
    ];
    if let Some(distance) = outcome.distance_km {
        lines.push(format!("- 距离：{distance:.2} km"));
    }
    if let Some(status_info) = &outcome.status_info {
        lines.push(format!("- 服务端结论：{status_info}"));
    }
    if outcome.cheat_reasons.is_empty() {
        lines.push("- 判违规依据：服务端未在逐条规则里给出".to_owned());
    } else {
        lines.push("- 判违规依据：".to_owned());
        for reason in &outcome.cheat_reasons {
            lines.push(format!("  · {reason}"));
        }
    }
    if !outcome.incomplete_reasons.is_empty() {
        lines.push("- 没达标（不算违规）的项：".to_owned());
        for reason in &outcome.incomplete_reasons {
            lines.push(format!("  · {reason}"));
        }
    }
    match outcome.can_appeal {
        Some(0) => lines.push("- 申诉：**通道已关闭**（不能再申诉）".to_owned()),
        Some(_) => lines.push("- 申诉：可以申诉（App 里点）".to_owned()),
        None => lines.push("- 申诉：服务端未说明".to_owned()),
    }
    if let Some(appeal_info) = &outcome.appeal_info {
        lines.push(format!("- 申诉说明：{appeal_info}"));
    }
    lines.push(String::new());
    lines.push(
        "这条是**提交之后**被回扫判的（提交当时六项规则都过了）—— 服务端的跑批是事后做的。\
         记着一条：提交成功不等于安全。"
            .to_owned(),
    );
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome() -> ReviewOutcome {
        ReviewOutcome {
            rrid: 1371568189,
            flagged: true,
            cheat_reasons: vec!["运动数据有问题".to_owned()],
            can_appeal: Some(0),
            appeal_info: Some("申诉未通过，查看审核信息".to_owned()),
            status_info: Some("你的跑步成绩已被系统判为可疑".to_owned()),
            incomplete_reasons: vec![],
            distance_km: Some(3.0),
            start_time: Some(1_795_215_459_000),
        }
    }

    #[test]
    fn notice_transcribes_server_facts_only() {
        let text = render_review_notice(&outcome());
        assert!(text.contains("运动数据有问题"), "{text}");
        assert!(text.contains("通道已关闭"), "{text}");
        assert!(text.contains("1371568189"), "{text}");
        assert!(text.contains("3.00 km"), "{text}");
        // 不许自己编因果：正文里不该出现「因为轨迹」这类解释。
        assert!(!text.contains("因为轨迹"), "{text}");
        assert!(!text.contains("说明轨迹"), "{text}");
    }

    #[test]
    fn review_is_scheduled_three_days_after_submit() {
        assert_eq!(REVIEW_AFTER_SECS, 3 * 86_400);
    }
}
