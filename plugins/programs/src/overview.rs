//! 项目计划总览 markdown 渲染：GSoC 当季时间线（近 7 天 ~ 未来 120 天）/
//! LFX 当前开放申请项目 / OSPP 静态卡；纯函数便于测试。

use ru41::adapters::sources::competition::gsoc::{GsocProgram, Milestone, format_cn};
use ru41::adapters::sources::feed::FeedItem;
use chrono::NaiveDate;

/// 时间线窗口：刚过去 7 天内的节点保留作上下文，120 天外的等临近再列。
const PAST_SECS: i64 = 7 * 86_400;
const HORIZON_SECS: i64 = 120 * 86_400;

pub fn render(gsoc: Option<&GsocProgram>, lfx: &[FeedItem], today: NaiveDate) -> String {
    let mut out = format!(
        "# 🧩 项目总览\n\n\
         > 生成于 {today} · 由 ru41 programs 附属每 6 小时刷新\n\
         > 口径：校外可个人申请的带薪开源项目计划，不看学校不看年级，能申就申；\n\
         > GSoC / OSPP 结项有钱，LFX 期次分 paid/unpaid 需逐个确认\n\n"
    );
    render_gsoc(&mut out, gsoc);
    render_lfx(&mut out, lfx);
    render_ospp(&mut out);
    out
}

fn render_gsoc(out: &mut String, program: Option<&GsocProgram>) {
    out.push_str("## GSoC（Google Summer of Code）\n\n");
    out.push_str(
        "> 谷歌主办的全球开源暑期计划，结项有津贴；contributor 申请通常每年 3 月开放、4 月截止。\n\n",
    );
    let Some(p) = program else {
        out.push_str("当季项目未公布或未激活，关注官网：https://summerofcode.withgoogle.com/\n\n");
        return;
    };
    out.push_str(&format!(
        "当前季：**GSoC {}**（阶段 `{}`）\n\n",
        p.year, p.phase
    ));

    // 申请窗口内置顶提醒。
    let now = ru41::util::now();
    let open = find_ts(p, "Contributor Proposals Open");
    let deadline = find_ts(p, "Proposal Deadline");
    if let (Some(open), Some(deadline)) = (open, deadline)
        && now >= open
        && now <= deadline
    {
        out.push_str(&format!(
            "🔥 **Contributor 申请进行中**，截止 {}，错过等一年！\n\n",
            format_cn(deadline)
        ));
    }

    let mut ms: Vec<&Milestone> = p
        .milestones
        .iter()
        .filter(|m| m.ts >= now - PAST_SECS && m.ts <= now + HORIZON_SECS)
        .collect();
    ms.sort_by_key(|m| m.ts);
    if ms.is_empty() {
        out.push_str("近 120 天暂无关键节点。\n\n");
        return;
    }
    out.push_str("| 时间 | 节点 |\n|---|---|\n");
    for m in ms {
        out.push_str(&format!("| {} | {} |\n", format_cn(m.ts), cell(&m.label)));
    }
    out.push_str("\n🔗 官网：https://summerofcode.withgoogle.com/\n\n");
}

fn find_ts(program: &GsocProgram, label: &str) -> Option<i64> {
    program
        .milestones
        .iter()
        .find(|m| m.label == label)
        .map(|m| m.ts)
}

/// 表格单元格消毒：竖线转义、换行压成空格（API 返回的简介常带 \n，会直接撑破表格）。
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
        .replace(['\n', '\r'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_lfx(out: &mut String, items: &[FeedItem]) {
    out.push_str("## LFX Mentorship（Linux 基金会导师计划）\n\n");
    out.push_str("> 全年滚动，导师带做真实开源项目；当前开放申请的项目如下。\n\n");
    if items.is_empty() {
        out.push_str("当前无开放申请的项目。\n\n");
        return;
    }
    out.push_str("| 项目 | 详情 |\n|---|---|\n");
    for it in items {
        let name = cell(
            it.title
                .strip_prefix("LFX Mentorship：")
                .unwrap_or(&it.title),
        );
        let summary = ru41::util::truncate_chars(&cell(&it.summary), 160);
        out.push_str(&format!("| [{name}]({}) | {summary} |\n", it.link));
    }
    out.push_str(&format!("\n共 {} 个开放项目\n\n", items.len()));
}

fn render_ospp(out: &mut String) {
    out.push_str(
        "## OSPP 开源之夏\n\n\
         > 中科院软件所主办的学生开源暑期计划，结项有奖金（往年基础 8000 / 进阶 12000 元）。\n\
         > 官网无公开 API，以下为往年节奏（推断，以官网公告为准）：\n\n\
         - 3 月：社区与项目列表上线，盯官网\n\
         - 4–5 月：学生申请窗口\n\
         - 6 月：中选公示\n\
         - 7–9 月：开发期，结项评审后发放奖金\n\n\
         🔗 官网：https://summer-ospp.ac.cn\n",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ru41::adapters::sources::competition::gsoc::GsocProgram;
    use ru41::adapters::sources::competition::gsoc::Milestone;

    fn program() -> GsocProgram {
        GsocProgram {
            year: "2027".into(),
            is_active: true,
            phase: "program_announce".into(),
            milestones: vec![
                Milestone {
                    label: "Program Announce Date".into(),
                    datetime: String::new(),
                    ts: ru41::util::now() - 86_400, // 昨天，窗口内
                },
                Milestone {
                    label: "Contributor Proposals Open".into(),
                    datetime: String::new(),
                    ts: ru41::util::now() + 30 * 86_400,
                },
                Milestone {
                    label: "Proposal Deadline".into(),
                    datetime: String::new(),
                    ts: ru41::util::now() + 45 * 86_400,
                },
                Milestone {
                    label: "Far Future Node".into(),
                    datetime: String::new(),
                    ts: ru41::util::now() + 200 * 86_400, // 超出窗口
                },
            ],
        }
    }

    #[test]
    fn renders_gsoc_timeline_within_window() {
        let p = program();
        let out = render(
            Some(&p),
            &[],
            NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
        );
        assert!(out.contains("GSoC 2027"));
        assert!(out.contains("Program Announce Date"), "{out}");
        assert!(out.contains("Contributor Proposals Open"), "{out}");
        assert!(!out.contains("Far Future Node"), "{out}");
        // 申请窗口未开（open 在 30 天后），不应有置顶提醒。
        assert!(!out.contains("申请进行中"), "{out}");
    }

    #[test]
    fn renders_application_callout_inside_window() {
        let mut p = program();
        p.milestones[1].ts = ru41::util::now() - 86_400; // open 昨天
        let out = render(
            Some(&p),
            &[],
            NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
        );
        assert!(out.contains("🔥 **Contributor 申请进行中**"), "{out}");
    }

    #[test]
    fn renders_lfx_table_and_empty_states() {
        let item = FeedItem {
            id: "lfx-x".into(),
            title: "LFX Mentorship：Hyphae APIS".into(),
            link: "https://mentorship.lfx.linuxfoundation.org/project/x".into(),
            summary: "Hyphae · 期次 SPRING 2027\n带换行的简介\r\n第二行".into(),
            published: "2026-10-27".into(),
        };
        let out = render(
            None,
            &[item],
            NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
        );
        assert!(
            out.contains("[Hyphae APIS](https://mentorship.lfx.linuxfoundation.org/project/x)")
        );
        assert!(out.contains("共 1 个开放项目"));
        // 换行必须被压平，否则 markdown 表格会破。
        assert!(out.contains("带换行的简介 第二行"), "{out}");
        assert!(!out.contains("带换行的简介\n"), "{out}");
        assert!(out.contains("当季项目未公布或未激活"), "{out}");

        let empty = render(None, &[], NaiveDate::from_ymd_opt(2026, 10, 27).unwrap());
        assert!(empty.contains("当前无开放申请的项目"));
        assert!(empty.contains("OSPP 开源之夏"));
    }
}
