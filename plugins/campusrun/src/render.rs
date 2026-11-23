//! 把控制台数据渲染成人读的 Markdown。
//!
//! 纯函数，不触网，方便单测锁住「用户到底会看到什么」。

use crate::model::{
    CheatStatus, FencesData, PlanData, PointsData, RunRecord, SemesterInfo, StartTimes, Verdict,
    ViolationDetail,
};
use chrono::{Local, TimeZone};

/// 学期进度。
pub fn render_status(info: &SemesterInfo) -> String {
    let summary = &info.summary_raw;
    let mut lines = vec!["## 校园跑进度".to_owned()];

    let name = summary
        .sname
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("（学期名未提供）");
    lines.push(format!("- 学期：{name}"));

    let total = summary.semester_dis.unwrap_or_default();
    let valid = summary.semester_valid_dis.unwrap_or_default();
    let ratio = summary
        .valid_ratio
        .map(|r| format!("，有效率 {:.1}%", r * 100.0))
        .unwrap_or_default();
    lines.push(format!(
        "- 有效里程：{}（共 {}{}）",
        fmt_distance(valid),
        fmt_distance(total),
        ratio
    ));

    let total_count = summary.semester_count.unwrap_or_default();
    let valid_count = summary.semester_valid_count.unwrap_or_default();
    lines.push(format!(
        "- 有效次数：{valid_count} 次（共 {total_count} 次）"
    ));
    // 里程与次数**两个口径不同源**（上游没解释）：实测有 4 条记录 complete=true，
    // 但学期「有效次数」仍是 0。所以这里只摆事实，**不猜原因**——
    // 上游没解释的东西，转述时也不能替它编一个。
    //
    // 另一条独立事实（来自记录详情，11-16 那条）：单次距离低于当次任务的最小距离
    // （selDistance，各任务不同）→ 该条 complete=false。
    if valid_count == 0 && valid > 0.0 {
        lines.push(
            "- ℹ️ 里程在涨，但「有效次数」是 0 —— 这两个口径不同源，**服务端没解释**；\
             次数口径以 App 为准。另外：单次距离要达到当次任务的最小距离\
             （记录详情里的 selDistance，各任务不同），否则**那一条**会被判未完成。"
                .to_owned(),
        );
    }

    // 目标：**只有上游的 semesterTargetDis / semesterTargetCount 算数**。
    //
    // ⚠️ 2026-11-23 实测（这条把之前「已达标」的误判堵上）：
    // 真实响应里 semesterTargetDis 与 semesterTargetCount **都是空的**，
    // 只剩一个 semesterGoal=1 —— 它的单位是未定义的：控制台当公里读、
    // pace-forge 当米读，两边自相矛盾（1 公里 vs 1 米）。
    // 而学期真实要求是几十公里量级 → 这个 1 根本不是公里目标。
    //
    // 所以：**拿它算「达标」就是错的**（之前正是这么报的「已达标」）。
    // 拿不到权威目标时，只如实报「服务端没给公里目标」，达标与否让用户按 App 核对。
    let goal_raw = info.goal_raw.as_ref();
    let authoritative_dis = goal_raw.and_then(|g| g.semester_target_dis).filter(|v| *v > 0.0);
    match authoritative_dis {
        Some(target_m) => {
            lines.push(format!(
                "- 学期目标里程：{}（服务端 semesterTargetDis）",
                fmt_goal_distance(target_m)
            ));
            let valid = summary.semester_valid_dis.unwrap_or_default();
            lines.push(format!(
                "- 达标判定：有效 {} ≥ 目标 {} → {}",
                fmt_distance(valid),
                fmt_goal_distance(target_m),
                if valid >= target_m {
                    "已达标"
                } else {
                    "未达标"
                }
            ));
        }
        None => {
            let raw_goal = goal_raw.and_then(|g| g.semester_goal);
            match raw_goal {
                Some(value) => lines.push(format!(
                    "- 学期目标：**服务端未给出公里目标**——semesterGoal={value}，\
                     而上游这个字段的单位是**米**(pace-forge 源码 api/semester.rs,\
                     实测样本 60000 即 60 km), 也就是 {value} 米 ≈ 这个课\
                     **没设记分跑学期目标**; semesterTargetDis/semesterTargetCount 都是空的"
                )),
                None => lines.push("- 学期目标：服务端未提供".to_owned()),
            }
            lines.push(
                "- ⚠️ **不能据此说「已达标」**：接口没给公里目标，学期跑量要求以 App 上的\
                 学期要求为准".to_owned(),
            );
        }
    }
    if let Some(count) = info.goal.as_ref().and_then(|g| g.target_count) {
        lines.push(format!(
            "- 学期目标次数：{count}{}",
            if count == 0 { "（不计次）" } else { "" }
        ));
    }
    if let Some(days) = info.goal.as_ref().and_then(|g| g.days_left) {
        lines.push(format!("- 学期剩余：{days} 天"));
    }
    if info.goal.as_ref().and_then(|g| g.in_term) == Some(false) {
        lines.push("- ⚠️ 当前不在学期内（成绩不计入本学期）".to_owned());
    }
    lines.join("\n")
}

/// 最近若干条记录。
pub fn render_records(records: &[RunRecord], limit: usize) -> String {
    let mut lines = vec!["## 校园跑最近记录".to_owned()];
    if records.is_empty() {
        lines.push("- 还没有记录".to_owned());
        return lines.join("\n");
    }
    let shown: Vec<&RunRecord> = records.iter().take(limit.max(1)).collect();
    lines.push("| 时间 | 距离 | 配速 | 有效 | 时长 | 步频 | 状态 |".to_owned());
    lines.push("| --- | --- | --- | --- | --- | --- | --- |".to_owned());
    for record in shown.iter() {
        let distance = record.total_dis.unwrap_or_default();
        let seconds = record.total_time.unwrap_or_default();
        let valid = record.valid_dis.unwrap_or(distance);
        let pace = pace_per_km(distance, seconds).unwrap_or_else(|| "—".to_owned());
        let valid_text = if valid + 0.5 < distance {
            format!("{}/{}", fmt_distance(valid), fmt_distance(distance))
        } else {
            fmt_distance(valid)
        };
        let step = record
            .avg_step_freq
            .filter(|n| *n > 0)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_owned());
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            fmt_datetime(record.start_time),
            fmt_distance(distance),
            pace,
            valid_text,
            fmt_duration(seconds),
            step,
            record_state(record),
        ));
    }
    if records.len() > shown.len() {
        lines.push(format!(
            "\n共 {} 条，这里只列最近 {} 条。",
            records.len(),
            shown.len()
        ));
    }
    lines.join("\n")
}

/// 违规名单/风控状态。
///
/// ⚠️ 服务端原始响应里是**两回事**：`self` = 自己的违规信息（null = 干净），
/// `list` = **全校通报榜**（别人）。控制台把 `list` 压成了 `listCount`，
/// 渲染时必须说清这是全校人数，否则会被读成「你有 9 条」。
pub fn render_cheat(cheat: &CheatStatus) -> String {
    render_cheat_with(cheat, &[])
}

/// 带逐条真实原因的账号状态。
///
/// 聚合接口的 `reason` 常年是「服务端未提供原因」，看起来像查不到；实际
/// 判罚依据在每条记录详情的 reasonList 里（`type=0` 那条）。所以这里要
/// 把 `violations` 展开，并把「没达标」与「判违规」分开说 —— 混在一起会把
/// 只是距离不够的记录说成违规。
pub fn render_cheat_with(cheat: &CheatStatus, violations: &[ViolationDetail]) -> String {
    let mut lines = vec!["## 校园跑账号状态".to_owned()];
    let label = cheat
        .label
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("（服务端未给标签）");
    lines.push(format!("- 你的违规记录：{label}（{}）", cheat.status));
    if let Some(count) = cheat.count {
        lines.push(format!("- 你的违规条数：{count}"));
    }
    if let Some(list) = cheat.list_count {
        lines.push(format!(
            "- 全校违规通报榜：{list} 人（**全校通报名单，是别人的记录，与你无关**）"
        ));
    }
    if let Some(reason) = cheat.reason.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- 原因（聚合层）：{reason}"));
    }
    if let Some(appeal) = cheat.appeal.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- 申诉：{appeal}"));
    }
    if let Some(occurred) = cheat
        .occurred_at
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        lines.push(format!("- 时间：{occurred}"));
    }
    // 「未知」不等于清白——报成清白比不报更糟。
    if cheat.status == "unknown" {
        lines.push("- ⚠️ 服务端字段缺失或异常，**不等于清白**，请到 App 里核对".to_owned());
    }
    if cheat.status == "stale" {
        lines.push("- ⚠️ 查询结果已过期（超过 5 分钟），请重查".to_owned());
    }
    lines.push(format!("- 查询时间：{}", fmt_datetime(cheat.checked_at_ms)));
    if !violations.is_empty() {
        lines.push(String::new());
        lines.push("### 逐条真实原因（从记录详情的 reasonList 挖出来的）".to_owned());
        for item in violations {
            lines.push(String::new());
            let when = item
                .start_time
                .map(|ms| fmt_datetime(Some(ms)))
                .unwrap_or_else(|| "时间未知".to_owned());
            let distance = item
                .distance_km
                .map(|km| format!(" · {km:.2} km"))
                .unwrap_or_default();
            lines.push(format!("- **rrid {}** · {when}{distance}", item.rrid));
            if let Some(info) = item.status_info.as_deref() {
                lines.push(format!("  - 服务端结论：{info}"));
            }
            if item.cheat_reasons.is_empty() {
                lines.push("  - 判违规依据：**逐条规则里没有「运动数据有问题」这类判罚项**".to_owned());
            } else {
                for reason in &item.cheat_reasons {
                    lines.push(format!("  - ❌ 判违规依据：**{reason}**"));
                }
            }
            if !item.incomplete_reasons.is_empty() {
                lines.push(format!(
                    "  - 没达标（不算违规）：{}",
                    item.incomplete_reasons.join(" / ")
                ));
            }
            if !item.passed_reasons.is_empty() {
                lines.push(format!("  - 已通过：{}", item.passed_reasons.join(" / ")));
            }
            if let Some(info) = item.appeal_info.as_deref() {
                let tail = match item.can_appeal {
                    Some(0) => "（`canAppeal=0`，**不能再申诉**）",
                    _ => "",
                };
                lines.push(format!("  - 申诉：{info}{tail}"));
            }
        }
    }
    lines.join("\n")
}

/// 一页看完：进度 + 最近记录 + 账号状态。
pub fn render_all(status: &SemesterInfo, records: &[RunRecord], cheat: &CheatStatus) -> String {
    format!(
        "{}\n\n{}\n\n{}",
        render_status(status),
        render_records(records, 5),
        render_cheat(cheat)
    )
}

/// 记录状态文案。
///
/// ⚠️ 只说**服务端给了什么**，不替它下结论：
/// - `complete`：控制台也拿它当有效性标志（`r.complete ?? …`）→ 说「已完成/未完成」；
/// - `status`：控制台的 JS **根本不读这个字段**，接口也没说明各取值的含义 →
///   非 0 时只报状态码，不说成「异常」；
/// - `appealStatus` / `stepFreqStatus`：字段名自解释，非 0 时照名字说，并带上原值。
fn record_state(record: &RunRecord) -> String {
    let mut parts = vec![if record.complete.unwrap_or(false) {
        "已完成".to_owned()
    } else {
        "未完成".to_owned()
    }];
    if let Some(status) = record.status.filter(|s| *s != 0) {
        parts.push(format!("服务端状态码 {status}（接口未说明含义）"));
    }
    if let Some(appeal) = record.appeal_status.filter(|s| *s != 0) {
        parts.push(format!("有申诉记录(appealStatus={appeal})"));
    }
    if let Some(freq) = record.step_freq_status.filter(|s| *s != 0) {
        parts.push(format!("步频状态异常(stepFreqStatus={freq})"));
    }
    parts.join("，")
}

/// 米 → km。
pub fn fmt_distance(meters: f64) -> String {
    format!("{:.2} km", meters / 1000.0)
}

/// 目标里程（米）：小于 1 公里按米显示，免得「0.00 km」看不出是什么。
pub fn fmt_goal_distance(meters: f64) -> String {
    if meters >= 1000.0 {
        fmt_distance(meters)
    } else {
        format!("{:.0} m", meters)
    }
}

/// 秒 → 配速（min/km）。
pub fn pace_per_km(distance_m: f64, seconds: f64) -> Option<String> {
    if distance_m <= 0.0 || seconds <= 0.0 {
        return None;
    }
    let pace_seconds = (seconds / (distance_m / 1000.0)).round() as i64;
    Some(format!(
        "{}'{:02}\"/km",
        pace_seconds / 60,
        pace_seconds % 60
    ))
}

/// 秒 → 时长。
pub fn fmt_duration(seconds: f64) -> String {
    let total = seconds.round().max(0.0) as i64;
    format!("{}'{:02}\"", total / 60, total % 60)
}

/// 毫秒时间戳 → `MM-DD HH:MM`。
pub fn fmt_datetime(ms: Option<i64>) -> String {
    match ms {
        Some(ms) => match Local.timestamp_millis_opt(ms).single() {
            Some(time) => time.format("%m-%d %H:%M").to_string(),
            None => "—".to_owned(),
        },
        None => "—".to_owned(),
    }
}

/// 毫秒时间戳 → `YYYY-MM-DD`（学期起止用）。
pub fn fmt_date(ms: Option<i64>) -> String {
    match ms {
        Some(ms) => match Local.timestamp_millis_opt(ms).single() {
            Some(time) => time.format("%Y-%m-%d").to_string(),
            None => "—".to_owned(),
        },
        None => "—".to_owned(),
    }
}

/// 上次提交的服务端判定（reasonList：逐条规则过没过）。
///
/// 这是**权威口径**：门槛各门课不一样（实测有「最少要跑1.00公里/跑7分钟以上」，
/// 也有「最少要跑1.50公里/跑6分钟以上」），所以规则只能从服务端读，不能写死。
pub fn render_verdict(verdict: &Verdict) -> String {
    let mut lines = vec!["## 上次提交的判定（服务端逐条规则）".to_owned()];
    for item in verdict.rules.iter() {
        let mark = if item.passed() { "✅" } else { "❌" };
        lines.push(format!(
            "- {mark} {}",
            item.reason.clone().unwrap_or_default()
        ));
    }
    if let Some(info) = verdict
        .status_info
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        lines.push(format!("结论：{info}"));
    }
    lines.join("\n")
}

/// 某条规则在上次提交里过没过（按规则文本找）。
pub fn rule_passed(verdict: &Verdict, keyword: &str) -> Option<bool> {
    verdict
        .rules
        .iter()
        .find(|item| {
            item.reason
                .as_deref()
                .is_some_and(|text| text.contains(keyword))
        })
        .map(|item| item.passed())
}

/// 校园跑规则面：补跑窗口、必过点、围栏、服务端当前门槛、上次判定。
pub fn render_rules(
    start_times: &StartTimes,
    points: &PointsData,
    fences: &FencesData,
    verdict: Option<&Verdict>,
) -> String {
    let mut lines = vec!["## 校园跑规则".to_owned()];
    // 服务端门槛：取上次那条记录用的规则（selDistance / 配速区间）。
    match verdict {
        Some(verdict) => {
            let mut bits = Vec::new();
            if let Some(min) = verdict.min_distance_km {
                bits.push(format!("最少 {}", fmt_goal_distance(min * 1000.0)));
            }
            if let Some((top, bottom)) = verdict.pace_range {
                bits.push(format!("配速 {top:.2}~{bottom:.2} 分/km"));
            }
            if bits.is_empty() {
                lines.push("- 服务端门槛：上次记录里没取到".to_owned());
            } else {
                lines.push(format!("- 服务端门槛（上次那条用的）：{}", bits.join("、")));
            }
        }
        None => lines.push("- 服务端门槛：还没有记录可比对".to_owned()),
    }
    if let Some(limits) = &start_times.limits {
        let mut bits = Vec::new();
        if let Some(days) = limits.max_days_ago {
            bits.push(format!("最多往前补 {days} 天"));
        }
        if let Some(lo) = limits.rand_hour_lo
            && let Some(hi) = limits.rand_hour_hi
        {
            bits.push(format!("随机时刻 {lo:02}:00~{hi:02}:00"));
        }
        if !bits.is_empty() {
            lines.push(format!("- 补跑限制：{}", bits.join("、")));
        }
    }
    if let (Some(start), Some(stop)) = (&start_times.start_text, &start_times.stop_text) {
        lines.push(format!("- 当前窗口：{start} ~ {stop}"));
    }
    let total = points.count.unwrap_or(points.points.len());
    if total == 0 {
        lines.push("- 必过点：服务端没给".to_owned());
    } else {
        lines.push(format!("- 必过点（{total} 个，全部要按顺序经过）："));
        for point in &points.points {
            let name = point.name.as_deref().unwrap_or("(未命名)");
            let fixed = if point.is_fixed.unwrap_or(0) == 1 {
                "（固定打卡点）"
            } else {
                ""
            };
            lines.push(format!("  - {name}{fixed}"));
        }
    }
    let fence_count = fences.count.unwrap_or(fences.fences.len());
    if fence_count == 0 {
        lines.push("- 围栏：服务端没给".to_owned());
    } else {
        for fence in &fences.fences {
            lines.push(format!(
                "- 围栏：{}（{} 个顶点，轨迹必须落在里面）",
                fence.name.as_deref().unwrap_or("(未命名)"),
                fence.ring.len()
            ));
        }
    }
    lines.join("\n")
}

/// 预演结论（未提交）。
pub fn render_plan(
    plan: &PlanData,
    params: &crate::submit::RunParams,
    verdict: Option<&Verdict>,
) -> String {
    let distance_km = params.distance_km;
    let duration_min = params.duration_min;
    let mut lines = vec!["## 校园跑预演（未提交）".to_owned()];
    let pace =
        pace_per_km(distance_km * 1000.0, duration_min * 60.0).unwrap_or_else(|| "—".to_owned());
    lines.push(format!(
        "- 参数：{distance_km:.2} km / {duration_min:.1} 分钟（配速 {pace}）"
    ));
    // 起跑时刻：**服务端给的**（含它有没有把时段收窄过），一并摆出来给用户看。
    let clamped = if params.start_clamped {
        "（服务端把这个时段收窄过）"
    } else {
        ""
    };
    lines.push(format!(
        "- 起跑：{}{clamped}",
        params
            .start_at_ms
            .map(|ms| fmt_datetime(Some(ms)))
            .unwrap_or_else(|| "现在（刚跑完）".to_owned())
    ));
    if let Some((lo, hi)) = params.alt_range {
        lines.push(format!("- 海拔：{lo:.0}~{hi:.0} m"));
    }
    let actual = plan.actual_length.unwrap_or_default();
    lines.push(format!(
        "- 实际里程：{} · 轨迹点 {} · 步数 {} · 平均步频 {}",
        fmt_distance(actual),
        plan.point_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into()),
        plan.steps
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into()),
        plan.avg_cadence
            .map(|n| format!("{n:.1} 步/分"))
            .unwrap_or_else(|| "—".into()),
    ));
    lines.push(format!(
        "- 打卡点：{}/{} 全过",
        plan.checkpoint_passed.unwrap_or(0),
        plan.checkpoint_total.unwrap_or(0)
    ));
    // 结论一律用**工具自己的**口径（data.ok / error / issueSummary），插件不另判一套。
    let blocked = plan.ok == Some(false);
    lines.push(format!(
        "- 工具自检：{}{}",
        plan.issue_summary.clone().unwrap_or_else(|| "未知".into()),
        if blocked {
            "（**没过，不能提交**）"
        } else {
            "（过了）"
        }
    ));
    if let Some(error) = plan.error.as_deref().filter(|text| !text.trim().is_empty()) {
        lines.push(format!("- 工具给的原因：{error}"));
    }
    // 穿点顺序：工具会给一条警告。上次提交这条规则判过没过，直接摆出来（别自己吓自己）。
    let order_warning: Vec<&String> = plan
        .logs
        .iter()
        .filter(|line| line.contains("point-order"))
        .collect();
    if !order_warning.is_empty() {
        let last = verdict.and_then(|item| rule_passed(item, "按顺序通过所有点位"));
        let tail = match last {
            Some(true) => "（上次提交这条规则判过了 ✅）",
            Some(false) => "（上次提交这条规则**没判过** ❌）",
            None => "（上次提交没有这条规则，过没过不知道）",
        };
        lines.push(format!("- 工具提示：穿点顺序与服务端次序不一致{tail}："));
        for line in order_warning {
            lines.push(format!("  {line}"));
        }
    }
    if let Some(mode) = &plan.track_mode {
        lines.push(format!("- 轨迹模式：{mode}"));
    }
    if let Some(min) = policy_min_distance(plan) {
        lines.push(format!("- 服务端最小里程：{}", fmt_goal_distance(min)));
    }
    for line in &plan.logs {
        lines.push(format!("  {line}"));
    }
    lines.join("\n")
}

/// 服务端 policy 的最小里程（米）。
///
/// 预演**成功**时服务端给结构化的 `policy.minDistance`；**失败**时那个字段没有，
/// 但工具自己的日志里有同一行策略（`√ [policy] … minDistance=1000 validTime=0`）——
/// 从那儿读，还是服务端说的数，不写死。
pub fn policy_min_distance(plan: &PlanData) -> Option<f64> {
    if let Some(min) = plan.policy.as_ref().and_then(|item| item.min_distance) {
        return Some(min);
    }
    plan.logs.iter().find_map(|line| {
        let tail = line.split_once("minDistance=")?.1;
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<f64>().ok()
    })
}

/// 工具自己的判定行（致命 / 警告）。
///
/// 预演失败时服务端只给一句「自检未通过：1 致命 / 0 警告 / 0 提示」，
/// 具体是哪条致命、哪条警告**全在 logs 里**——插件只转述，不自己重新判定。
pub fn tool_verdict_lines(plan: &PlanData) -> Vec<String> {
    plan.logs
        .iter()
        .map(|line| line.trim())
        .filter(|line| line.contains("致命") || line.contains("警告"))
        .map(|line| line.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SemesterGoal, SemesterGoalRaw, SemesterSummary};

    /// 2026-11-22 生产实测的真实数据：两条被判违规的记录，聚合接口只说
    /// 「原因：服务端未提供原因」，真正原因在 reasonList 的 type=0 那条。
    #[test]
    fn cheat_shows_real_violation_reason_from_reason_list() {
        let detail: crate::model::RunRecordDetail = serde_json::from_str(
            r#"{"rrid":1367213418,"totalDis":1212.0,"startTime":1794870446000,
                "statusInfo":"你的跑步成绩已被系统判为可疑","isCheat":1,
                "appealStatus":4,"appealStatusInfo":"申诉未通过，查看审核信息","canAppeal":0,
                "reasonList":[
                  {"complete":true,"reason":"按顺序通过所有点位","type":9},
                  {"complete":true,"reason":"最少要跑1.00公里","type":1},
                  {"complete":false,"reason":"运动数据有问题","type":0}
                ]}"#,
        )
        .unwrap();
        assert!(detail.is_flagged(), "isCheat=1 该算判违规");
        let violation = ViolationDetail::from_detail(&detail);
        assert_eq!(violation.cheat_reasons, vec!["运动数据有问题".to_owned()]);
        assert!(violation.incomplete_reasons.is_empty(), "门槛项不该算违规依据");

        let cheat: CheatStatus = serde_json::from_str(
            r#"{"status":"flagged","label":"服务端已标记违规","count":2,
                "reason":"服务端未提供原因","appeal":"未知",
                "occurredAt":"服务端未提供时间","listCount":20,"checkedAtMs":1795361991577}"#,
        )
        .unwrap();
        let text = render_cheat_with(&cheat, &[violation]);
        assert!(text.contains("运动数据有问题"), "{text}");
        assert!(text.contains("判违规依据"), "{text}");
        assert!(text.contains("不能再申诉"), "{text}");
        assert!(text.contains("已通过：按顺序通过所有点位"), "{text}");
        // 聚合层的「未提供原因」要与逐条原因并存，不能互相覆盖
        assert!(text.contains("服务端未提供原因"), "{text}");
    }

    /// 距离不够只是「这次不成功」，**不能**渲染成判违规。
    #[test]
    fn cheat_keeps_incomplete_reasons_out_of_violation() {
        let detail: crate::model::RunRecordDetail = serde_json::from_str(
            r#"{"rrid":1367168510,"totalDis":1216.0,"startTime":1794809703000,
                "statusInfo":"你的跑步成绩已被系统判为可疑","isCheat":1,"canAppeal":0,
                "reasonList":[
                  {"complete":false,"reason":"最少要跑1.50公里","type":1},
                  {"complete":true,"reason":"跑6分钟以上","type":7},
                  {"complete":false,"reason":"运动数据有问题","type":0}
                ]}"#,
        )
        .unwrap();
        let violation = ViolationDetail::from_detail(&detail);
        assert_eq!(violation.incomplete_reasons, vec!["最少要跑1.50公里".to_owned()]);
        assert_eq!(violation.cheat_reasons, vec!["运动数据有问题".to_owned()]);
        assert_eq!(violation.passed_reasons, vec!["跑6分钟以上".to_owned()]);
    }

    /// 没被判违规的记录不展开（别把正常记录也说成违规）。
    #[test]
    fn clean_record_is_not_flagged() {
        let detail: crate::model::RunRecordDetail = serde_json::from_str(
            r#"{"rrid":1371568189,"totalDis":3000.0,"status":0,
                "reasonList":[{"complete":true,"reason":"按顺序通过所有点位","type":9}]}"#,
        )
        .unwrap();
        assert!(!detail.is_flagged());
    }

    /// 服务端把起跑时段**收窄**过时要转述给用户（控制台这时会弹窗确认）。
    #[test]
    fn plan_reports_server_side_clamping() {
        let plan: PlanData =
            serde_json::from_str(r#"{"ok":true,"actualLength":2000,"checkpointPassed":5,"checkpointTotal":5}"#)
                .unwrap();
        let base = crate::submit::base_params(
            &crate::config::CampusrunSection::default(),
            &crate::submit::RunIntent::DistanceKm { km: 2.0 },
            1_794_890_000_000,
        )
        .unwrap();
        let params = crate::submit::plan_params(
            base,
            crate::submit::StartAt {
                at_ms: Some(1_794_890_000_000),
                clamped: true,
            },
        );
        let text = render_plan(&plan, &params, None);
        assert!(text.contains("起跑："), "{text}");
        assert!(text.contains("服务端把这个时段收窄过"), "{text}");
    }

    /// 没被收窄时**不要**出现收窄提示（别制造没发生过的事）。
    #[test]
    fn plan_does_not_invent_clamping() {
        let plan: PlanData =
            serde_json::from_str(r#"{"ok":true,"actualLength":2000,"checkpointPassed":5,"checkpointTotal":5}"#)
                .unwrap();
        let base = crate::submit::base_params(
            &crate::config::CampusrunSection::default(),
            &crate::submit::RunIntent::DistanceKm { km: 2.0 },
            1_794_890_000_000,
        )
        .unwrap();
        let params = crate::submit::plan_params(
            base,
            crate::submit::StartAt {
                at_ms: Some(1_794_890_000_000),
                clamped: false,
            },
        );
        let text = render_plan(&plan, &params, None);
        assert!(!text.contains("收窄"), "{text}");
    }

    /// 最小里程：预演成功时取服务端给的结构化字段。
    #[test]
    fn policy_min_distance_reads_structured_field() {
        let plan: PlanData =
            serde_json::from_str(r#"{"ok":true,"policy":{"minDistance":1000,"validTime":0}}"#).unwrap();
        assert_eq!(policy_min_distance(&plan), Some(1000.0));
    }

    /// 预演**失败**时服务端不给结构化字段，但同一行策略在工具日志里——
    /// 还是服务端说的数，不写死。
    #[test]
    fn policy_min_distance_falls_back_to_the_tool_log() {
        let plan: PlanData = serde_json::from_str(
            r#"{"ok":false,"logs":["√ [policy] ts=1794901127456 policy=1 minDistance=1000 validTime=0"]}"#,
        )
        .unwrap();
        assert_eq!(policy_min_distance(&plan), Some(1000.0));
    }

    /// 工具自己的致命/警告行要原样带出来（插件不重新判定）。
    #[test]
    fn tool_verdict_lines_are_relayed_verbatim() {
        let plan: PlanData = serde_json::from_str(
            r#"{"ok":false,"logs":[
                "√ [clock] 已校时",
                "  ✗ 致命 [CP_MISS] 打卡点 P5 最近距离 37.1m，未命中（要求 ≤0.5m）",
                "  ! 警告 [PACE_FAST] 配速 2.25 分/km 快得离谱"
            ]}"#,
        )
        .unwrap();
        let lines = tool_verdict_lines(&plan);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("CP_MISS"), "{lines:?}");
        assert!(lines[1].contains("PACE_FAST"), "{lines:?}");
        // 「√ [clock] 已校时」不是判定行，别混进来。
        assert!(!lines.iter().any(|line| line.contains("clock")), "{lines:?}");
    }

    fn summary() -> SemesterSummary {
        SemesterSummary {
            semester_dis: Some(7511.0),
            semester_valid_dis: Some(6295.0),
            sname: Some("2026-2027-1体能训练Ⅰ".into()),
            semester_count: Some(0),
            semester_valid_count: Some(0),
            valid_ratio: Some(0.8381041139661829),
            text: Some("服务端自己拼的那句话".into()),
        }
    }

    fn info() -> SemesterInfo {
        SemesterInfo {
            summary_raw: summary(),
            goal_raw: Some(SemesterGoalRaw {
                // semesterGoal 是**目标里程（米）**：1 米 = 实质没有门槛。
                semester_goal: Some(1.0),
                semester_target_dis: None,
                semester_target_count: None,
                semester_valid: Some(0.0),
                goal_method: Some(2),
                start_date: Some(1_793_376_000_000),
                end_date: Some(1_798_819_199_000),
                sname: None,
            }),
            goal: Some(SemesterGoal {
                target_dis: Some(1.0),
                target_count: Some(0),
                days_left: Some(45),
                in_term: Some(true),
            }),
        }
    }

    #[test]
    fn status_refuses_to_claim_done_without_authoritative_goal() {
        let text = render_status(&info());
        // 上游没给 semesterTargetDis → **不许**说「已达标」。
        // (2026-11-23 实测: semesterGoal=1 的单位未定义, 拿它算达标是错的)
        assert!(text.contains("服务端未给出公里目标"), "{text}");
        assert!(text.contains("不能据此说「已达标」"), "{text}");
        // 判定行（带箭头的那种）不该存在 —— 警告文案里出现「已达标」不算
        assert!(!text.contains("→ 已达标"), "{text}");
        assert!(text.contains("学期目标次数：0（不计次）"), "{text}");
        assert!(text.contains("学期剩余：45 天"), "{text}");
        // 旧措辞（「学期目标：1 次」「还差 1 次」「目标距离：1.00 km」）是错的。
        assert!(!text.contains("还差"), "{text}");
        assert!(!text.contains("目标距离"), "{text}");
    }

    /// 上游**给了**权威目标（semesterTargetDis）时，才算达标 —— 这时口径才可信。
    #[test]
    fn status_uses_authoritative_target_when_present() {
        let mut info = info();
        let goal_raw = info.goal_raw.as_mut().expect("有 goal_raw");
        goal_raw.semester_target_dis = Some(30_000.0);
        let text = render_status(&info);
        assert!(text.contains("学期目标里程：30.00 km"), "{text}");
        assert!(text.contains("→ 未达标"), "{text}");
        assert!(!text.contains("→ 已达标"), "{text}");
    }

    /// 里程与次数口径不同源 → 只摆事实，**不猜原因**（服务端没解释的东西不能替它编）。
    #[test]
    fn status_reports_count_and_distance_without_inventing_a_cause() {
        let text = render_status(&info());
        assert!(text.contains("「有效次数」是 0"), "{text}");
        assert!(text.contains("这两个口径不同源"), "{text}");
        // 不许把「单次距离不够」说成次数为 0 的原因
        assert!(!text.contains("说明这些记录**没被判完成**"), "{text}");
    }

    #[test]
    fn pace_and_duration_format() {
        assert_eq!(pace_per_km(1212.0, 426.0).as_deref(), Some("5'51\"/km"));
        assert_eq!(pace_per_km(0.0, 100.0), None);
        assert_eq!(fmt_duration(426.0), "7'06\"");
        assert_eq!(fmt_distance(1212.0), "1.21 km");
    }

    #[test]
    fn record_state_flags_anomalies() {
        let mut record = RunRecord {
            rrid: 1,
            uuid: None,
            sport_type: Some(1),
            total_dis: Some(1212.0),
            total_time: Some(426.0),
            valid_dis: Some(1212.0),
            valid_time: Some(426.0),
            start_time: Some(1_794_870_446_000),
            stop_time: None,
            complete: Some(true),
            status: Some(0),
            appeal_status: Some(0),
            step_freq_status: Some(0),
            avg_step_freq: Some(158),
            distance_km: Some(1.21),
        };
        assert_eq!(record_state(&record), "已完成");
        record.status = Some(1);
        // 状态码只报码，不替服务端说「异常」。
        assert!(record_state(&record).contains("服务端状态码 1"));
        assert!(!record_state(&record).contains("异常(1)"));
        record.appeal_status = Some(2);
        assert!(record_state(&record).contains("有申诉记录"));
    }

    #[test]
    fn cheat_unknown_is_not_reported_as_clean() {
        let cheat = CheatStatus {
            status: "unknown".into(),
            label: Some("状态未知".into()),
            count: None,
            reason: None,
            appeal: None,
            occurred_at: None,
            rrid: None,
            checked_at_ms: Some(1_794_871_590_236),
            list_count: Some(9),
        };
        let text = render_cheat(&cheat);
        assert!(text.contains("不等于清白"), "{text}");
    }

    /// listCount 是**全校通报榜人数**，不是你的违规条数——
    /// 服务端原始响应里 self（你的违规信息）与 list（全校榜）是两回事。
    #[test]
    fn cheat_list_count_is_labelled_as_school_wide() {
        let cheat = CheatStatus {
            status: "no_record".into(),
            label: Some("暂未发现违规记录".into()),
            count: None,
            reason: Some("服务端未提供原因".into()),
            appeal: Some("未知".into()),
            occurred_at: Some("服务端未提供时间".into()),
            rrid: None,
            checked_at_ms: Some(1_794_871_590_236),
            list_count: Some(9),
        };
        let text = render_cheat(&cheat);
        assert!(text.contains("你的违规记录：暂未发现违规记录"), "{text}");
        assert!(text.contains("全校违规通报榜：9 人"), "{text}");
        assert!(text.contains("与你无关"), "{text}");
        // 旧措辞「服务端通报条数」容易被读成自己有 9 条，别再出现。
        assert!(!text.contains("通报条数"), "{text}");
    }

    #[test]
    fn records_table_lists_recent_rows() {
        let records = vec![RunRecord {
            rrid: 1367213418,
            uuid: Some("u".into()),
            sport_type: Some(1),
            total_dis: Some(1212.0),
            total_time: Some(426.0),
            valid_dis: Some(1000.0),
            valid_time: Some(400.0),
            start_time: Some(1_794_870_446_000),
            stop_time: Some(1_794_870_872_000),
            complete: Some(true),
            status: Some(0),
            appeal_status: Some(0),
            step_freq_status: Some(0),
            avg_step_freq: Some(158),
            distance_km: Some(1.21),
        }];
        let text = render_records(&records, 5);
        // 时间列不能写死「11-17 07:07」：渲染用的是 Local，机器时区一变
        // 断言就假失败（2026-11-22 实测：会话切到 UTC-8 后差 16 小时）。
        // 改成与同一份 fmt_datetime 对齐，验的是「日期取自 start_time」这件事。
        let expected_when = fmt_datetime(Some(1_794_870_446_000));
        assert!(
            text.contains(&format!("| {expected_when} | 1.21 km | 5'51\"/km |")),
            "{text}"
        );
        // 有效 < 总：两个数都显示，别让人误以为整条都算。
        assert!(text.contains("1.00 km/1.21 km"), "{text}");
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;
    use crate::model::ReasonItem;

    fn item(reason: &str, complete: bool) -> ReasonItem {
        ReasonItem {
            complete: Some(complete),
            complete_status: Some(if complete { 1 } else { 0 }),
            reason: Some(reason.to_owned()),
            kind: None,
        }
    }

    /// 「跑7分钟以上」这条规则本身照原样转述（不解析、不换算、不改判）。
    #[test]
    fn rule_lines_are_relayed_verbatim() {
        let verdict = Verdict {
            rrid: 1,
            filename: None,
            rules: vec![
                item("按顺序通过所有点位", true),
                item("最少要跑1.00公里", true),
                item("跑7分钟以上", true),
            ],
            status_info: None,
            min_distance_km: Some(1.0),
            pace_range: Some((2.17, 10.0)),
        };
        assert_eq!(rule_passed(&verdict, "跑7分钟以上"), Some(true));
        let text = render_verdict(&verdict);
        assert!(text.contains("跑7分钟以上"), "{text}");
        assert!(text.contains("✅"), "{text}");
    }
}
