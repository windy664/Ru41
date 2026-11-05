//! 用量与可观测性报告：把「跑了多少次 / 真正花了多少 / 缓存省了多少」讲清楚。
//!
//! 两种版式：
//! - [`Style::Chat`]：QQ 消息。QQ 的 markdown 不支持表格，改用分组标题 + 列表。
//! - [`Style::Doc`]：写入文件 / Obsidian 的完整版，带表格与分隔线。
//!
//! 口径约定（两个版式一致，别再让人对着数字猜）：
//! - **账面总 Token** = 新读入 + 缓存命中 + 输出（三项合计）。
//! - **实际消耗** = 新读入 + 输出。缓存命中单价远低于普通输入，所以不算进「花的钱」。
//! - 底座没上报 usage 的运行只进「运行次数 / 成功率 / 耗时」，不进 token 列。

use crate::app::usage::{UsageOverview, UsageStat};

/// 报告版式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// QQ 消息：不用表格。
    Chat,
    /// 文档 / 笔记：表格 + 分节。
    Doc,
}

/// 报告的统计窗口与分组数据。
pub struct Dashboard<'a> {
    /// 近 24 小时。
    pub day: &'a UsageOverview,
    /// 近 7 天（主口径）。
    pub week: &'a UsageOverview,
}

/// 实际消耗 token：新读入 + 输出。
pub fn real_tokens(stat: &UsageStat) -> u64 {
    stat.input_tokens + stat.output_tokens
}

/// 中文口径的 token 数：一万以上用「万」，保留两位小数再去掉多余的 0。
pub fn cn_tokens(count: u64) -> String {
    if count >= 10_000 {
        let text = format!("{:.2}", count as f64 / 10_000.0);
        let trimmed = text.trim_end_matches('0').trim_end_matches('.');
        format!("{trimmed}万")
    } else {
        with_commas(count)
    }
}

/// 千分位。
pub fn with_commas(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "—".to_owned();
    }
    format!("{:.1}%", part as f64 / whole as f64 * 100.0)
}

/// 一组用量的人类口径：真实消耗 + 缓存。
fn stat_phrase(stat: &UsageStat) -> String {
    if stat.reported_runs == 0 {
        "未上报".to_owned()
    } else {
        format!(
            "真实 {} · 缓存 {}",
            cn_tokens(real_tokens(stat)),
            cn_tokens(stat.cached_input_tokens)
        )
    }
}

/// 生成报告。
pub fn render(week: &UsageOverview, day: &UsageOverview, style: Style) -> String {
    let dashboard = Dashboard { day, week };
    match style {
        Style::Doc => render_doc(&dashboard),
        Style::Chat => render_chat(&dashboard),
    }
}

/// 真实消耗（新读入 + 输出）。
fn real_of(overview: &UsageOverview) -> u64 {
    overview.input_tokens + overview.output_tokens
}

/// 来源分组：已知来源给出中文名，其余一次性任务名（冒烟测试、临时任务）合并成一档。
fn group_sources(items: &[UsageStat]) -> Vec<(String, UsageStat)> {
    let mut known: Vec<(String, UsageStat)> = Vec::new();
    let mut other: Option<UsageStat> = None;
    for item in items {
        let label = crate::app::usage::source_label(&item.key);
        if label.is_empty() {
            let bucket = other.get_or_insert_with(|| UsageStat {
                key: "other".to_owned(),
                runs: 0,
                reported_runs: 0,
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                succeeded: 0,
                failed: 0,
            });
            bucket.runs += item.runs;
            bucket.reported_runs += item.reported_runs;
            bucket.input_tokens += item.input_tokens;
            bucket.cached_input_tokens += item.cached_input_tokens;
            bucket.output_tokens += item.output_tokens;
            bucket.total_tokens += item.total_tokens;
            bucket.succeeded += item.succeeded;
            bucket.failed += item.failed;
        } else {
            known.push((format!("{label}（{}）", item.key), item.clone()));
        }
    }
    if let Some(bucket) = other {
        known.push(("其他 / 临时任务".to_owned(), bucket));
    }
    known
}

// ---------------------------------------------------------------- 文档版

fn render_doc(dashboard: &Dashboard<'_>) -> String {
    let week = dashboard.week;
    let real = real_of(week);
    let mut out = String::new();

    out.push_str("# 📊 RU41 · 用量与可观测性报告\n\n");
    out.push_str("<sub>AGENT RUNTIME / TOKEN ANALYTICS / TASK MONITORING</sub>\n\n");
    out.push_str(&format!(
        "**统计周期：** 近 24 小时（主口径为近 7 天）  \n**运行规模：** {} 次运行 / {} 次 Token 上报（近 7 天）\n\n",
        week.runs, week.reported_runs
    ));
    out.push_str("---\n\n");

    // 01 核心用量
    out.push_str("## 01 · 核心用量\n\n");
    out.push_str("| 指标 | 数值 |\n|:---|---:|\n");
    out.push_str(&format!(
        "| 账面总 Token | {} |\n",
        cn_tokens(week.total_tokens)
    ));
    out.push_str(&format!("| 实际消耗 Token | **{}** |\n", cn_tokens(real)));
    out.push_str(&format!(
        "| 缓存命中 Token | {} |\n",
        cn_tokens(week.cached_input_tokens)
    ));
    out.push_str(&format!(
        "| 缓存命中率 | {} |\n",
        percent(week.cached_input_tokens, week.total_tokens)
    ));
    out.push_str(&format!("| Token 上报次数 | {} |\n\n", week.reported_runs));

    out.push_str("### 实际消耗构成\n\n");
    out.push_str("| 类型 | Token | 占比 |\n|:---|---:|---:|\n");
    out.push_str(&format!(
        "| 新读入 Input | {} | {} |\n",
        cn_tokens(week.input_tokens),
        percent(week.input_tokens, real)
    ));
    out.push_str(&format!(
        "| 输出 Output | {} | {} |\n",
        cn_tokens(week.output_tokens),
        percent(week.output_tokens, real)
    ));
    out.push_str(&format!(
        "| **实际消耗** | **{}** | **100%** |\n\n",
        cn_tokens(real)
    ));
    out.push_str(&format!(
        "> **{} Tokens** 是按新读入与输出口径统计的实际消耗量，不包含缓存命中量。\n\n",
        cn_tokens(real)
    ));

    // 02 Token 账单
    out.push_str("## 02 · Token 账单\n\n");
    out.push_str("| 统计口径 | Token | 说明 |\n|:---|---:|:---|\n");
    out.push_str(&format!(
        "| 新读入 (Input) | {} | 模型第一次读取的内容 |\n",
        cn_tokens(week.input_tokens)
    ));
    out.push_str(&format!(
        "| 缓存命中 (Cache Hit) | {} | 重复上下文命中缓存 |\n",
        cn_tokens(week.cached_input_tokens)
    ));
    out.push_str(&format!(
        "| 输出 (Output) | {} | 模型生成的内容 |\n",
        cn_tokens(week.output_tokens)
    ));
    out.push_str(&format!(
        "| **账面总量 (Total)** | **{}** | 三项合计 |\n",
        cn_tokens(week.total_tokens)
    ));
    out.push_str(&format!(
        "| **实际消耗 (Input + Output)** | **{}** | 不含缓存命中 |\n\n",
        cn_tokens(real)
    ));
    out.push_str("### 缓存效率\n\n");
    out.push_str(&format!(
        "- 缓存命中率：**{}**（缓存命中 ÷ 账面总量）\n- 缓存命中 **{}**，按供应商的低单价档位计费，因此不计入实际消耗\n- 实际消耗 **{}** 里，模型新读入 {}、生成 {}\n\n",
        percent(week.cached_input_tokens, week.total_tokens),
        cn_tokens(week.cached_input_tokens),
        cn_tokens(real),
        cn_tokens(week.input_tokens),
        cn_tokens(week.output_tokens)
    ));

    // 03 运行表现
    out.push_str("## 03 · 运行表现\n\n");
    out.push_str("| 指标 | 数值 |\n|:---|---:|\n");
    out.push_str(&format!("| 总运行次数 | {} |\n", week.runs));
    out.push_str(&format!("| 成功次数 | {} |\n", week.succeeded));
    out.push_str(&format!("| 失败次数 | {} |\n", week.failed));
    out.push_str(&format!(
        "| 成功率 | {} |\n",
        percent(u64::from(week.succeeded), u64::from(week.runs))
    ));
    out.push_str(&format!("| 平均耗时 | {}s |\n", week.avg_seconds));
    out.push_str(&format!("| 最长耗时 | {}s |\n\n", week.max_seconds));

    // 04 模型分布
    if !week.by_model.is_empty() {
        out.push_str("## 04 · 模型分布\n\n");
        out.push_str("| 模型 | 运行次数 | 成功 | Token |\n|:---|---:|---:|---:|\n");
        for item in &week.by_model {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                item.key,
                item.runs,
                item.succeeded,
                stat_phrase(item)
            ));
        }
        out.push('\n');
    }

    // 05 功能来源
    let sources = group_sources(&week.by_source);
    if !sources.is_empty() {
        out.push_str("## 05 · 功能来源\n\n");
        out.push_str("| 功能来源 | 运行次数 | 成功 | Token |\n|:---|---:|---:|---:|\n");
        for (name, item) in &sources {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                name,
                item.runs,
                item.succeeded,
                stat_phrase(item)
            ));
        }
        out.push('\n');
    }

    // 06 每日趋势
    if !week.by_day.is_empty() {
        out.push_str("## 06 · 每日趋势\n\n");
        out.push_str("| 日期 | 运行次数 | Token 上报 | 实际消耗 |\n|:---|---:|---:|---:|\n");
        for item in &week.by_day {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                item.key,
                item.runs,
                item.reported_runs,
                stat_phrase(item)
            ));
        }
        out.push('\n');
    }

    // 07 数据完整性
    out.push_str("## 07 · 数据完整性\n\n");
    out.push_str(&format!(
        "- 近 7 天共 {} 次运行，其中 {} 次上报了 Token Usage。\n",
        week.runs, week.reported_runs
    ));
    if week.runs > week.reported_runs {
        out.push_str(&format!(
            "- 另外 {} 次运行没有可用的 Token 统计数据：底座走 chat 接口时不返回 usage 字段。\n",
            week.runs - week.reported_runs
        ));
    }
    out.push_str("- 未上报不等于消耗为零；运行次数、成功率、耗时基于实际运行记录。\n");
    out.push_str("- 缓存命中与普通输入分开核算，不能按同一单价计算。\n\n");

    // 08 任务追踪
    out.push_str("## 08 · 任务追踪\n\n");
    out.push_str("```bash\nru41 task trace <任务号>\n```\n\n");
    out.push_str("可用于排查任务执行过程、失败原因、模型调用及耗时。\n");

    out
}

// ---------------------------------------------------------------- QQ 版

fn render_chat(dashboard: &Dashboard<'_>) -> String {
    let week = dashboard.week;
    let real = real_of(week);
    let mut lines: Vec<String> = Vec::new();

    lines.push("## 📊 Token 用量".to_owned());
    lines.push(String::new());
    lines.push(format!(
        "**近 24 小时** · {} 次运行 · {}",
        dashboard.day.runs,
        if dashboard.day.reported_runs == 0 {
            "未上报 token".to_owned()
        } else {
            format!("真实消耗 {} token", cn_tokens(real_of(dashboard.day)))
        }
    ));
    lines.push(format!(
        "**近 7 天** · {} 次运行 · {}",
        week.runs,
        if week.reported_runs == 0 {
            "未上报 token".to_owned()
        } else {
            format!("真实消耗 {} token", cn_tokens(real))
        }
    ));
    if week.reported_runs > 0 {
        lines.push(format!(
            "　└ 新输入 {} · 输出 {} · 缓存命中 {} · 账面合计 {}",
            cn_tokens(week.input_tokens),
            cn_tokens(week.output_tokens),
            cn_tokens(week.cached_input_tokens),
            cn_tokens(week.total_tokens)
        ));
        lines.push(format!(
            "　└ 缓存命中率 {} · 实际消耗占比：输入 {} / 输出 {}",
            percent(week.cached_input_tokens, week.total_tokens),
            percent(week.input_tokens, real),
            percent(week.output_tokens, real)
        ));
    }
    lines.push(format!(
        "**成功率** · {}/{} 次成功 · 失败 {} 次 · 平均 {}s（最长 {}s）",
        week.succeeded, week.runs, week.failed, week.avg_seconds, week.max_seconds
    ));

    if !week.by_model.is_empty() {
        lines.push(String::new());
        lines.push("**按模型**".to_owned());
        for item in &week.by_model {
            lines.push(format!(
                "- `{}` · {} 次 · 成功 {} · {}",
                item.key,
                item.runs,
                item.succeeded,
                stat_phrase(item)
            ));
        }
    }
    let sources = group_sources(&week.by_source);
    if !sources.is_empty() {
        lines.push(String::new());
        lines.push("**按功能来源**".to_owned());
        for (name, item) in &sources {
            lines.push(format!(
                "- {name} · {} 次 · 成功 {} · {}",
                item.runs,
                item.succeeded,
                stat_phrase(item)
            ));
        }
    }
    if !week.by_day.is_empty() {
        lines.push(String::new());
        lines.push("**近 7 天趋势**".to_owned());
        for item in &week.by_day {
            lines.push(format!(
                "- {} · {} 次 · {}",
                item.key,
                item.runs,
                stat_phrase(item)
            ));
        }
    }
    lines.push(String::new());
    lines.push(
        "> 真实消耗 = 新读入 + 输出，是你真正计费的部分；缓存命中是同一段上下文重复发给模型时命中了服务商缓存，单价远低于普通输入，所以「账面合计」看着吓人但花钱不多。".to_owned(),
    );
    if week.runs > week.reported_runs {
        lines.push(format!(
            "> ℹ️ {} 次运行没有 token 数据：底座走 chat 接口时不返回 usage，所以 token 列不出来；运行次数、成功率、耗时都是实的。\n想看某次运行具体做了什么：`ru41 task trace <任务号>`",
            week.runs - week.reported_runs
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::usage::UsageStat;

    fn stat(key: &str, runs: u32, reported: u32) -> UsageStat {
        UsageStat {
            key: key.to_owned(),
            runs,
            reported_runs: reported,
            input_tokens: if reported > 0 { 154 } else { 0 },
            cached_input_tokens: if reported > 0 { 10_500 } else { 0 },
            output_tokens: if reported > 0 { 850 } else { 0 },
            total_tokens: if reported > 0 { 11_500 } else { 0 },
            succeeded: runs.saturating_sub(1),
            failed: 1,
        }
    }

    fn overview() -> UsageOverview {
        UsageOverview {
            days: 7,
            runs: 123,
            reported_runs: 1,
            input_tokens: 154,
            cached_input_tokens: 10_500,
            output_tokens: 850,
            total_tokens: 11_500,
            by_model: vec![stat("pi/minimax/MiniMax-M3", 6, 1)],
            by_source: vec![stat("qq", 6, 1), stat("lecture", 24, 0)],
            by_day: vec![stat("11-05", 30, 1)],
            succeeded: 120,
            failed: 3,
            avg_seconds: 42,
            max_seconds: 300,
        }
    }

    #[test]
    fn cn_tokens_uses_wan_and_keeps_precision() {
        assert_eq!(cn_tokens(1_004), "1,004");
        assert_eq!(cn_tokens(10_500), "1.05万");
        assert_eq!(cn_tokens(137_964), "13.8万");
        assert_eq!(cn_tokens(318_788), "31.88万");
    }

    #[test]
    fn doc_report_shows_real_and_cache_separately() {
        let week = overview();
        let text = render(&week, &week, Style::Doc);
        // 主指标是实际消耗，不是账面总量。
        assert!(text.contains("实际消耗 Token | **1,004**"), "{text}");
        assert!(text.contains("缓存命中 Token | 1.05万"), "{text}");
        assert!(text.contains("账面总 Token | 1.15万"), "{text}");
        // 表格版式。
        assert!(text.contains("| 指标 | 数值 |"), "{text}");
        // 未上报的分组要写明。
        assert!(text.contains("未上报"), "{text}");
    }

    #[test]
    fn chat_report_has_no_tables_and_explains_real_tokens() {
        let week = overview();
        let text = render(&week, &week, Style::Chat);
        assert!(!text.contains('|'), "QQ 版不该有表格：{text}");
        assert!(text.contains("真实消耗 1,004 token"), "{text}");
        assert!(text.contains("缓存命中率 91.3%"), "{text}");
        assert!(text.contains("QQ 消息（qq）"), "{text}");
        assert!(text.contains("未上报"), "{text}");
    }
}
