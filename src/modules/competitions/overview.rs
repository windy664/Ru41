//! 日程总览 markdown 渲染：大湾区 ⭐ 在前、按报名截止升序；纯函数便于测试。

use super::filter::CompetitionRow;
use chrono::NaiveDate;

pub fn render(rows: &[CompetitionRow], today: NaiveDate) -> String {
    let mut out = format!(
        "# 📅 竞赛日程总览\n\n\
         > 生成于 {today} · 由 ru41 competitions 附属每日刷新，过期赛事自动移出\n\
         > 口径：必须有奖（现金/奖品/权益，纯证书不收）× 大湾区线下 ⭐ / 国内线上 / 国外线上；\n\
         > 剔除中小学生赛与已截止；同名跨源去重\n\n\
         | 报名截止 | 赛事 | 地点/形式 | 奖池/奖励 | 题材/赛道 | 开赛 | 来源 |\n\
         |---|---|---|---|---|---|---|\n"
    );
    for row in rows {
        let name = row.name.replace('|', "\\|");
        let star = if row.gba { "⭐ " } else { "" };
        let deadline = row
            .deadline
            .map(|d| d.to_string())
            .unwrap_or_else(|| "—".into());
        let start = row
            .start
            .map(|d| d.to_string())
            .unwrap_or_else(|| "—".into());
        let themes = if row.themes.is_empty() {
            "—".to_owned()
        } else {
            row.themes.join("、").replace('|', "\\|")
        };
        out.push_str(&format!(
            "| {deadline} | {star}[{name}]({}) | {} | {} | {} | {start} | {} |\n",
            row.link, row.place, row.prize, themes, row.source
        ));
    }
    out.push_str(&format!("\n共 {} 条\n", rows.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_table_with_gba_star_and_themes() {
        let row = CompetitionRow {
            deadline: NaiveDate::from_ymd_opt(2026, 11, 15),
            name: "测试黑客松".into(),
            place: "深圳·线下".into(),
            prize: "总奖金 10 万元".into(),
            start: None,
            link: "https://example.com".into(),
            source: "HackerTrip",
            themes: vec!["AI 应用".into()],
            gba: true,
        };
        let out = render(&[row], NaiveDate::from_ymd_opt(2026, 10, 26).unwrap());
        assert!(out.contains("⭐ [测试黑客松](https://example.com)"));
        assert!(out.contains("AI 应用"));
        assert!(out.contains("共 1 条"));
        assert!(out.contains("2026-11-15"));
    }

    #[test]
    fn empty_renders_zero_count() {
        let out = render(&[], NaiveDate::from_ymd_opt(2026, 10, 26).unwrap());
        assert!(out.contains("共 0 条"));
    }
}
