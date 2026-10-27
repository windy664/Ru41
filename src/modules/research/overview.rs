//! 科研总览渲染：静态项目卡 + 申请窗口倒计时（按往年节奏推断，纯函数便于测试）。
//! 这些项目没有公开 API，倒计时的意义是"别让一年一次的窗口悄悄过去"；
//! 真实开窗由 pagewatch 盯官网页面变更确认。

use chrono::{Datelike, NaiveDate};

/// 一个可个人申请的科研计划。窗口为往年节奏（推断），None = 全年滚动。
pub struct ResearchProgram {
    pub name: &'static str,
    pub org: &'static str,
    /// 往年申请窗口：(开窗 月-日, 关窗 月-日)。
    pub window: Option<((u32, u32), (u32, u32))>,
    pub summary: &'static str,
    pub url: &'static str,
}

/// 项目清单：只收"不依赖本校推荐、个人可申"的。
const PROGRAMS: &[ResearchProgram] = &[
    ResearchProgram {
        name: "Mitacs Globalink 研究实习",
        org: "加拿大 · 全额资助",
        window: Some(((8, 5), (9, 16))), // 2026 季实际窗口：8/5–9/16
        summary: "12 周加拿大大学实验室暑研，机票/住宿/保险/津贴全包。中国本科生个人网申，不看学校；评审看 GPA、英语、项目经历。建议大二/大三申请（次年 5–7 月成行）。",
        url: "https://www.mitacs.ca/our-programs/globalink-research-internship-students/",
    },
    ResearchProgram {
        name: "HKUST 暑期本科生研究计划（SRP）",
        org: "香港科技大学",
        window: Some(((1, 15), (3, 31))), // 推断：项目列表秋末放出，申请约 1–3 月
        summary: "跟港科教授做 4–8 周研究项目，部分院系给津贴+住宿（数学/物理/生命科学等系明确有）。走 Visiting Intern 个人申请通道，同时是 PhD 提前批面试入口。",
        url: "https://summercampus.hkust.edu.hk/",
    },
    ResearchProgram {
        name: "港校各系 PhD 夏令营 / 暑研",
        org: "HKU / CUHK / HKUST 各院系",
        window: Some(((12, 1), (4, 30))), // 推断：各系 12 月–次年 4 月不等
        summary: "各院系自行组织，本质是 PhD 提前批：表现好直接拿 offer。主要面向升大三/大四的学生（你的主场在 2028–2029 申请季）。各系页面分散，盯目标院系官网。",
        url: "https://www.cse.ust.hk/",
    },
    ResearchProgram {
        name: "中科院「科创计划」",
        org: "中国科学院各研究所",
        window: Some(((3, 1), (5, 31))), // 推断：各所春季陆续发布
        summary: "大学生创新实践训练计划，个人向目标研究所/导师申请，进所跟课题。各所时间与名额不一，直接盯感兴趣的研究所官网「研究生教育/招生」栏目。",
        url: "https://www.cas.cn/",
    },
    ResearchProgram {
        name: "AI 实验室科研实习（上海 AI Lab / 之江等）",
        org: "各地 AI 实验室",
        window: None, // 全年滚动
        summary: "全年滚动招聘研究实习生，走实习投递渠道（我们的岗位总览已覆盖这类口径）。也可以直接盯实验室官网的 Careers/Join Us 页面。",
        url: "https://www.shlab.org.cn/",
    },
];

pub fn render(today: NaiveDate) -> String {
    let mut out = format!(
        "# 🔬 科研总览\n\n\
         > 生成于 {today} · 由 ru41 research 附属每 6 小时刷新倒计时\n\
         > 口径：校外可**个人申请**的暑研/科研实习，不走学校推荐；窗口均按往年节奏推断，以官网公告为准\n\
         > 双保险：pagewatch 盯官网变更 + 关键窗口种了日程提醒，开窗会主动喊你\n\
         > 与 Programs-项目计划的区别：那边是开源项目制（GSoC/LFX/开源之夏），这边是学术研究经历\n\n\
         | 项目 | 机构 | 申请窗口（往年推断） | 倒计时 |\n\
         |---|---|---|---|\n"
    );
    for p in PROGRAMS {
        out.push_str(&format!(
            "| [{}]({}) | {} | {} | {} |\n",
            p.name,
            p.url,
            p.org,
            window_label(p.window),
            countdown_label(p.window, today)
        ));
    }
    out.push_str("\n## 说明与申请要点\n\n");
    for p in PROGRAMS {
        out.push_str(&format!("- **{}**：{}\n", p.name, p.summary));
    }
    out.push_str(
        "\n> ⚠️ 这些项目大多看 GPA/排名、英语成绩（雅思/托福/CET-6）与项目作品集；\
         窗口日期全是往年推断，官宣时间以 pagewatch 提醒为准。\n",
    );
    out
}

/// 窗口文本："8月5日 – 9月16日" / "全年滚动"。
fn window_label(window: Option<((u32, u32), (u32, u32))>) -> String {
    match window {
        Some(((om, od), (cm, cd))) => format!("{om}月{od}日 – {cm}月{cd}日"),
        None => "全年滚动".into(),
    }
}

/// 倒计时文本：窗口进行中 / 距开窗约 N 天 / 今年已过（明年约 M 月）。
/// 跨年窗口（如 12月–次年4月）：关窗月小于开窗月时，关窗算到下一年。
fn countdown_label(window: Option<((u32, u32), (u32, u32))>, today: NaiveDate) -> String {
    let Some(((om, od), (cm, cd))) = window else {
        return "随时可投".into();
    };
    let crosses_year = (cm, cd) <= (om, od);
    // 进行中判定：检查「今年开窗的周期」与「去年开窗的周期」（跨年窗口覆盖上半年）。
    for open_year in [today.year(), today.year() - 1] {
        let Some(open) = NaiveDate::from_ymd_opt(open_year, om, od) else {
            continue;
        };
        let close_year = if crosses_year { open_year + 1 } else { open_year };
        let Some(close) = NaiveDate::from_ymd_opt(close_year, cm, cd) else {
            continue;
        };
        if today >= open && today <= close {
            return "🔥 窗口进行中（推断）".into();
        }
    }
    // 下一次开窗：今年的还没到就数日子，过了就指向明年。
    if let Some(open) = NaiveDate::from_ymd_opt(today.year(), om, od)
        && today < open
    {
        let days = (open - today).num_days();
        return format!("距开窗约 {days} 天");
    }
    format!("今年窗口已过，下次约 {} 年 {om} 月", today.year() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MITACS: Option<((u32, u32), (u32, u32))> = Some(((8, 5), (9, 16)));

    #[test]
    fn countdown_before_during_after_window() {
        // 窗口前（2027-03-01 → 距 8/5 约 157 天）
        let before = countdown_label(MITACS, NaiveDate::from_ymd_opt(2027, 3, 1).unwrap());
        assert!(before.starts_with("距开窗约"), "{before}");
        // 窗口中
        let during = countdown_label(MITACS, NaiveDate::from_ymd_opt(2027, 8, 20).unwrap());
        assert!(during.contains("窗口进行中"), "{during}");
        // 窗口后 → 指向明年
        let after = countdown_label(MITACS, NaiveDate::from_ymd_opt(2027, 10, 1).unwrap());
        assert!(after.contains("2028 年 8 月"), "{after}");
        // 全年滚动
        assert_eq!(countdown_label(None, NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()), "随时可投");
        // 跨年窗口（12/1–次年4/30）：1 月中应在窗口内，10 月应数日子
        let crossing = Some(((12, 1), (4, 30)));
        let mid = countdown_label(crossing, NaiveDate::from_ymd_opt(2027, 1, 15).unwrap());
        assert!(mid.contains("窗口进行中"), "{mid}");
        let before_cross = countdown_label(crossing, NaiveDate::from_ymd_opt(2026, 10, 27).unwrap());
        assert!(before_cross.starts_with("距开窗约"), "{before_cross}");
    }

    #[test]
    fn renders_all_programs_with_links() {
        let out = render(NaiveDate::from_ymd_opt(2026, 10, 27).unwrap());
        assert!(out.contains("Mitacs Globalink"));
        assert!(out.contains("HKUST 暑期本科生研究计划"));
        assert!(out.contains("科创计划"));
        assert!(out.contains("mitacs.ca"));
        // 今天（10-27）Mitacs 窗口已过 → 指向 2027 年 8 月
        assert!(out.contains("2027 年 8 月"), "{out}");
    }
}
