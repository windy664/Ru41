//! 岗位总览渲染：实习/校招两节，发布日倒序，大湾区 ⭐ 标注。

use super::filter::{JobEntry, is_gba, posted_md};

pub fn render(entries: &[JobEntry], today: chrono::NaiveDate) -> String {
    let mut rows: Vec<&JobEntry> = entries.iter().collect();
    rows.sort_by_key(|e| std::cmp::Reverse(posted_md(&e.posted).unwrap_or((0, 0))));

    let intern: Vec<&&JobEntry> = rows.iter().filter(|e| e.kind == "实习").collect();
    let campus: Vec<&&JobEntry> = rows.iter().filter(|e| e.kind != "实习").collect();

    let mut out = String::new();
    out.push_str("# 岗位总览\n\n");
    out.push_str(&format!(
        "> 由 ru41 每 6 小时刷新（数据源：面灵 AI 职位表 + BOSS 直聘 sidecar，含跨轮合并，发布超 30 天自动消失）。\n> 最近刷新：{}\n\n",
        today.format("%Y-%m-%d")
    ));
    out.push_str("**口径**：实习 + 校招都留（年级不作过滤）；只留技术岗（AI/大模型/Agent/算法/后端/安全/Infra/数据/开发），产品/运营/销售等非技术岗剔除；⭐ = 大湾区。\n\n");

    out.push_str(&format!("## 实习（{} 条）\n\n", intern.len()));
    render_table(&mut out, &intern);
    out.push_str(&format!("\n## 校招（{} 条）\n\n", campus.len()));
    render_table(&mut out, &campus);

    let with_jd: Vec<&&JobEntry> = rows.iter().filter(|e| e.jd.is_some()).collect();
    if !with_jd.is_empty() {
        out.push_str(&format!("\n## JD 详情（{} 条）\n\n", with_jd.len()));
        out.push_str("> 面灵条目回源官方招聘站公开 API 抓取（ashby / greenhouse / smartrecruiters）；BOSS 条目由 sidecar 详情接口直接带全文；其余国内平台（飞书招聘/美团/腾讯/京东）接口待适配，暂缺。\n\n");
        for e in with_jd {
            out.push_str(&format!("### {} · {}\n\n", e.company, e.role));
            out.push_str(&format!(
                "{} · {} · 发布 {}\n\n{}\n\n",
                e.kind,
                e.city,
                e.posted,
                e.jd.as_deref().unwrap_or("")
            ));
        }
    }
    out
}

fn render_table(out: &mut String, rows: &[&&JobEntry]) {
    out.push_str("| 发布 | 公司 | 岗位 | 城市 | 招聘对象 |\n");
    out.push_str("|---|---|---|---|---|\n");
    for e in rows {
        let city = if is_gba(&e.city) {
            format!("⭐ {}", e.city)
        } else {
            e.city.clone()
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            e.posted, e.company, e.role, city, e.audience
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_sections_sorted_desc_with_gba_star() {
        let entries = vec![
            JobEntry {
                company: "字节跳动".into(),
                role: "AI Agent开发实习生".into(),
                city: "广州 / 深圳".into(),
                audience: "日常实习".into(),
                kind: "实习".into(),
                posted: "10月24日".into(),
                platform: Some("feishu-hire".into()),
                jd: None,
                jd_tried: true,
            },
            JobEntry {
                company: "美团".into(),
                role: "数据开发工程师".into(),
                city: "北京".into(),
                audience: "未公布".into(),
                kind: "校招".into(),
                posted: "10月25日".into(),
                platform: Some("meituan".into()),
                jd: Some("负责数据平台研发……".into()),
                jd_tried: true,
            },
        ];
        let md = render(&entries, chrono::NaiveDate::from_ymd_opt(2026, 10, 26).unwrap());
        assert!(md.contains("## 实习（1 条）"));
        assert!(md.contains("## 校招（1 条）"));
        assert!(md.contains("⭐ 广州 / 深圳"));
        assert!(md.contains("| 10月24日 | 字节跳动 |"));
        assert!(md.contains("| 10月25日 | 美团 |"));
        assert!(md.contains("## JD 详情（1 条）"));
        assert!(md.contains("### 美团 · 数据开发工程师"));
    }
}
