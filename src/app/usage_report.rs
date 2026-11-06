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
use crate::bootstrap::config::PricingSection;

/// 一个模型的单价（美元 / 百万 token）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitPrice {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
}

impl UnitPrice {
    /// MiniMax-M3 的公开价（2026-11）：输入 0.30、输出 1.20、缓存命中 0.06。
    pub const MINIMAX_M3: UnitPrice = UnitPrice {
        input: 0.30,
        output: 1.20,
        cache_read: 0.06,
    };

    /// 按 token 数折算花费（美元）。
    pub fn cost(&self, input: u64, cache_read: u64, output: u64) -> f64 {
        (input as f64 / 1_000_000.0) * self.input
            + (cache_read as f64 / 1_000_000.0) * self.cache_read
            + (output as f64 / 1_000_000.0) * self.output
    }
}

/// 定价解析：配置优先，内置 MiniMax-M3 兜底。
pub struct Pricing {
    default: UnitPrice,
    models: Vec<(String, UnitPrice)>,
    /// 美元兑人民币。
    pub usd_per_cny: f64,
}

impl Pricing {
    /// 没有任何配置时的默认定价（MiniMax-M3）。
    pub fn builtin() -> Self {
        Pricing {
            default: UnitPrice::MINIMAX_M3,
            models: Vec::new(),
            usd_per_cny: 7.2,
        }
    }

    /// 从配置构造；模型名匹配忽略大小写与供应方前缀。
    pub fn from_config(section: &PricingSection) -> Self {
        let mut pricing = Pricing::builtin();
        if let Some(input) = section.input_per_million {
            pricing.default.input = input;
        }
        if let Some(output) = section.output_per_million {
            pricing.default.output = output;
        }
        if let Some(cache) = section.cache_read_per_million {
            pricing.default.cache_read = cache;
        }
        if let Some(rate) = section.usd_per_cny {
            pricing.usd_per_cny = rate;
        }
        for (name, model) in &section.models {
            pricing.models.push((
                name.clone(),
                UnitPrice {
                    input: model.input_per_million.unwrap_or(pricing.default.input),
                    output: model.output_per_million.unwrap_or(pricing.default.output),
                    cache_read: model
                        .cache_read_per_million
                        .unwrap_or(pricing.default.cache_read),
                },
            ));
        }
        pricing
    }

    /// 查某个模型的单价；查不到用默认。
    pub fn price_of(&self, model: &str) -> UnitPrice {
        let bare = model.rsplit_once('/').map_or(model, |(_, id)| id);
        for (name, price) in &self.models {
            let name_bare = name.rsplit_once('/').map_or(name.as_str(), |(_, id)| id);
            if name.eq_ignore_ascii_case(model) || name_bare.eq_ignore_ascii_case(bare) {
                return *price;
            }
        }
        self.default
    }

    /// 一组用量折算出的花费（美元）。
    ///
    /// `model` 为 None 时用默认价（按来源分组时组内可能混着多个模型，
    /// 单价差异大时以按模型分组的那张表为准）。
    pub fn cost_of(&self, stat: &UsageStat, model: Option<&str>) -> Option<f64> {
        if stat.reported_runs == 0 {
            return None;
        }
        let price = match model {
            Some(model) => self.price_of(model),
            None => self.default,
        };
        Some(price.cost(
            stat.input_tokens,
            stat.cached_input_tokens,
            stat.output_tokens,
        ))
    }
}

/// 花费格式化：小于 1 分钱时用「$0.000x」级别，不然四舍五入到分。
pub fn fmt_usd(cost: f64) -> String {
    if cost <= 0.0 {
        return "$0".to_owned();
    }
    if cost < 0.01 {
        return format!("${cost:.4}");
    }
    format!("${cost:.3}")
}

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
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
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
pub fn render(
    week: &UsageOverview,
    day: &UsageOverview,
    style: Style,
    pricing: &Pricing,
) -> String {
    let dashboard = Dashboard { day, week };
    match style {
        Style::Doc => render_doc(&dashboard),
        Style::Chat => render_chat(&dashboard, pricing),
    }
}

/// 终端显示宽度：CJK 与全角符号按 2 列算（等宽表格对齐靠它）。
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| {
            let cp = c as u32;
            if (0x1100..=0x115F).contains(&cp)
                || (0x2E80..=0xA4CF).contains(&cp)
                || (0xAC00..=0xD7A3).contains(&cp)
                || (0xF900..=0xFAFF).contains(&cp)
                || (0xFE30..=0xFE6F).contains(&cp)
                || (0xFF00..=0xFF60).contains(&cp)
                || (0xFFE0..=0xFFE6).contains(&cp)
                || (0x20000..=0x3FFFD).contains(&cp)
            {
                2
            } else {
                1
            }
        })
        .sum()
}

/// 按显示宽度右侧补空格。
fn pad(text: &str, width: usize) -> String {
    let w = display_width(text);
    let mut out = text.to_owned();
    for _ in w..width {
        out.push(' ');
    }
    out
}

/// 终端里的表格：等宽代码块（CJK 宽度对齐）。
/// QQ 的 markdown 不支持 markdown 表格，代码块是唯一能对齐的写法。
fn code_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let columns = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| display_width(h)).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate().take(columns) {
            widths[index] = widths[index].max(display_width(cell));
        }
    }
    let mut out = String::new();
    let header_line = headers
        .iter()
        .enumerate()
        .map(|(i, h)| pad(h, widths[i]))
        .collect::<Vec<_>>()
        .join("  ");
    out.push_str(header_line.trim_end());
    out.push('\n');
    let rule = widths
        .iter()
        .map(|w| "─".repeat(*w))
        .collect::<Vec<_>>()
        .join("  ");
    out.push_str(&rule);
    out.push('\n');
    for row in rows {
        let line = row
            .iter()
            .enumerate()
            .take(columns)
            .map(|(i, cell)| pad(cell, widths[i]))
            .collect::<Vec<_>>()
            .join("  ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim_end().to_owned()
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

fn render_chat(dashboard: &Dashboard<'_>, pricing: &Pricing) -> String {
    let week = dashboard.week;
    let day = dashboard.day;
    let mut lines: Vec<String> = Vec::new();

    lines.push("## 📊 Token 用量".to_owned());
    lines.push(String::new());

    // 总览表：两个窗口并排，花费是主角。
    let day_cost = day_cost(pricing, day);
    let week_cost = week_cost(pricing, week);
    let mut rows: Vec<Vec<String>> = Vec::new();
    rows.push(vec![
        "运行次数".to_owned(),
        day.runs.to_string(),
        week.runs.to_string(),
    ]);
    rows.push(vec![
        "实际花费".to_owned(),
        day_cost.map(fmt_usd).unwrap_or_else(|| "—".to_owned()),
        week_cost.map(fmt_usd).unwrap_or_else(|| "—".to_owned()),
    ]);
    rows.push(vec![
        "  新读入".to_owned(),
        cn_tokens(day.input_tokens),
        cn_tokens(week.input_tokens),
    ]);
    rows.push(vec![
        "  缓存命中".to_owned(),
        cn_tokens(day.cached_input_tokens),
        cn_tokens(week.cached_input_tokens),
    ]);
    rows.push(vec![
        "  输出".to_owned(),
        cn_tokens(day.output_tokens),
        cn_tokens(week.output_tokens),
    ]);
    rows.push(vec![
        "成功率".to_owned(),
        percent(u64::from(day.succeeded), u64::from(day.runs)),
        percent(u64::from(week.succeeded), u64::from(week.runs)),
    ]);
    rows.push(vec![
        "平均耗时".to_owned(),
        format!("{}s", day.avg_seconds),
        format!("{}s", week.avg_seconds),
    ]);
    lines.push(format!(
        "```\n{}\n```",
        code_table(&["指标", "近 24 小时", "近 7 天"], &rows)
    ));

    // 花费说明：让人知道这个钱怎么算出来的。
    let price = pricing.price_of("");
    lines.push(String::new());
    lines.push(format!(
        "> 花费按 MiniMax-M3 公开价折算：输入 ${:.2}/M、缓存命中 ${:.2}/M、输出 ${:.2}/M；缓存命中只按输入价的 1/5 计价，所以 token 数看着吓人、钱不多。",
        price.input, price.cache_read, price.output
    ));
    if let Some(cost) = week_cost {
        lines.push(format!(
            "> 近 7 天实际花费 **{}**（约 ¥{:.2}，汇率 {:.1}）",
            fmt_usd(cost),
            cost * pricing.usd_per_cny,
            pricing.usd_per_cny
        ));
    }

    // 按模型
    if !week.by_model.is_empty() {
        lines.push(String::new());
        lines.push("**按模型**".to_owned());
        let mut rows: Vec<Vec<String>> = Vec::new();
        for item in &week.by_model {
            let cost = pricing
                .cost_of(item, Some(&item.key))
                .map(fmt_usd)
                .unwrap_or_else(|| "—".to_owned());
            let tokens = if item.reported_runs == 0 {
                "—".to_owned()
            } else {
                cn_tokens(real_tokens(item))
            };
            rows.push(vec![item.key.clone(), item.runs.to_string(), tokens, cost]);
        }
        lines.push(format!(
            "```\n{}\n```",
            code_table(&["模型", "次数", "真实消耗", "花费"], &rows)
        ));
    }

    // 按功能来源
    let sources = group_sources(&week.by_source);
    if !sources.is_empty() {
        lines.push(String::new());
        lines.push("**按功能来源（近 7 天）**".to_owned());
        let mut rows: Vec<Vec<String>> = Vec::new();
        for (name, item) in &sources {
            let cost = pricing
                .cost_of(item, None)
                .map(fmt_usd)
                .unwrap_or_else(|| "—".to_owned());
            let tokens = if item.reported_runs == 0 {
                "—".to_owned()
            } else {
                cn_tokens(real_tokens(item))
            };
            rows.push(vec![name.clone(), item.runs.to_string(), tokens, cost]);
        }
        lines.push(format!(
            "```\n{}\n```",
            code_table(&["来源", "次数", "真实消耗", "花费"], &rows)
        ));
    }

    // 未上报说明
    if week.runs > week.reported_runs {
        lines.push(String::new());
        lines.push(format!(
            "> ℹ️ {} 次运行没有 token 数据（底座走 chat 接口时不返回 usage），所以花费算不出来；运行次数、成功率、耗时都是实的。\n想看某次运行具体做了什么：`ru41 task trace <任务号>`",
            week.runs - week.reported_runs
        ));
    }
    lines.join("\n")
}

/// 窗口的总花费（美元）；没有上报数据时 None。
fn window_cost(pricing: &Pricing, overview: &UsageOverview) -> Option<f64> {
    if overview.reported_runs == 0 {
        return None;
    }
    let price = pricing.price_of("");
    Some(price.cost(
        overview.input_tokens,
        overview.cached_input_tokens,
        overview.output_tokens,
    ))
}

fn day_cost(pricing: &Pricing, day: &UsageOverview) -> Option<f64> {
    window_cost(pricing, day)
}

fn week_cost(pricing: &Pricing, week: &UsageOverview) -> Option<f64> {
    window_cost(pricing, week)
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
        let text = render(&week, &week, Style::Doc, &Pricing::builtin());
        // 主指标是实际消耗，不是账面总量。
        assert!(text.contains("实际消耗 Token | **1,004**"), "{text}");
        assert!(text.contains("缓存命中 Token | 1.05万"), "{text}");
        assert!(text.contains("账面总 Token | 1.15万"), "{text}");
        // 表格版式。
        assert!(text.contains("| 指标 | 数值 |"), "{text}");
        // 未上报的分组要写明。
        assert!(text.contains("没有可用的 Token 统计数据"), "{text}");
        // Obsidian 版只讲用量，不掺花费（花费只在 QQ 的 /用量 里）。
        assert!(!text.contains("花费"), "{text}");
    }

    #[test]
    fn chat_report_has_no_tables_and_explains_real_tokens() {
        let week = overview();
        let text = render(&week, &week, Style::Chat, &Pricing::builtin());
        // QQ 版用等宽代码块当表格，不出现 markdown 表格竖线。
        assert!(!text.contains("| "), "QQ 版不该有 markdown 表格：{text}");
        // 表格化的总览：指标行 + 两个窗口。
        assert!(text.contains("指标"), "{text}");
        assert!(text.contains("近 24 小时"), "{text}");
        assert!(text.contains("近 7 天"), "{text}");
        assert!(text.contains("实际花费"), "{text}");
        assert!(text.contains("新读入"), "{text}");
        assert!(text.contains("缓存命中"), "{text}");
        assert!(text.contains("成功率"), "{text}");
        // 花费要给出单价口径，让人知道钱怎么算的。
        assert!(text.contains("缓存命中只按输入价的 1/5 计价"), "{text}");
        // 分来源表也带花费列。
        assert!(text.contains("QQ 消息（qq）"), "{text}");
        assert!(text.contains("没有 token 数据"), "{text}");
    }
}
