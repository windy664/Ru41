//! 长期规划文档：战略层产物的领域模型。
//!
//! ## 它在整条链路里的位置
//!
//! ```text
//!   人给方向（目标 / 权重 / 锚点）
//!        ↓
//!   [长期规划文档]  ← 人审过，存在 Vault 的 08-Planning-规划/Plans-计划/
//!        ↓  AI 拆解（agent 任务，见 planner 插件的 plan-decompose skill）
//!   带 step 的计划  ← 名字 + 时长
//!        ↓  确定性装箱（planner 插件 / availability.rs）
//!   具体日程
//!        ↓  执行
//!   进度回写长期规划文档  →  下一轮拆解读新进度
//! ```
//!
//! **关键**：短期任务拆解的输入是**长期规划文档**，不是待办清单。
//! 没有长期规划文档就没有短期任务 —— 这样拆出来的任务才有出处，
//! 也才能回答"这一步为什么做"。
//!
//! ## 人和 AI 的边界（写进文档格式里）
//!
//! - **人必须给**：`id` / `name` / `kind` / `weight` / `anchor` / 目标陈述 /
//!   验收标准 / 停止条件 / 里程碑标题与描述
//! - **AI 负责补**：每个里程碑下的 `steps`（名字 + 时长）
//! - **人审**：AI 补的 steps 要写回文档，人看过才算数
//!
//! 解析器会检查"该有的有没有"，缺了就报出来 —— 缺的东西就是 AI 该干的。

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// 长期规划文档在 Vault 里的目录（相对 vault 根）。
pub const PLAN_DOC_DIR: &str = "08-Planning-规划/Plans-计划";

/// 目标类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    /// 硬锚定脉冲型：考试 / 报名截止驱动，倒排
    Pulse,
    /// 跟课型：跟着校历周次走
    Course,
    /// 日常型：整块推进
    Daily,
}

impl PlanKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanKind::Pulse => "pulse",
            PlanKind::Course => "course",
            PlanKind::Daily => "daily",
        }
    }
}

/// 一个子任务（AI 拆解出来的一步）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub name: String,
    pub minutes: i64,
}

/// 长期规划文档的 frontmatter（人给的元数据）。
#[derive(Clone, Debug, PartialEq)]
pub struct PlanMeta {
    pub id: String,
    pub name: String,
    pub kind: PlanKind,
    /// 目标权重（人给的 0-100）。
    pub weight: f64,
    /// 硬锚点：考试日 / 报名截止（ISO 日期）。
    pub anchor: Option<String>,
    /// 跟课型每周固定占几个块。
    pub weekly_blocks: usize,
}

/// 一个里程碑：标题 + 描述（人写的）+ steps（AI 补的）。
#[derive(Clone, Debug, PartialEq)]
pub struct PlanMilestone {
    pub name: String,
    /// 里程碑描述（人写的自由文本）—— AI 拆解时最重要的依据。
    pub description: String,
    pub steps: Vec<PlanStep>,
}

/// 一篇长期规划文档的解析结果。
#[derive(Clone, Debug, PartialEq)]
pub struct LongTermPlan {
    pub meta: PlanMeta,
    /// 目标陈述（人写的）。
    pub goal: String,
    /// 验收标准（人写的）—— 决定「做完没有」。
    pub acceptance: Vec<String>,
    /// 停止条件（人写的）—— 决定「该不该放弃」。
    pub stop_conditions: Vec<String>,
    pub milestones: Vec<PlanMilestone>,
    /// 文档里写了但没解析出来的行（防止静默丢内容）。
    pub unparsed: Vec<String>,
}

impl LongTermPlan {
    /// 缺 steps 的里程碑名 —— 这些需要 AI 补。
    pub fn needs_llm_steps(&self) -> Vec<String> {
        self.milestones
            .iter()
            .filter(|m| m.steps.is_empty())
            .map(|m| m.name.clone())
            .collect()
    }

    /// 全部 step 的总分钟数。
    pub fn total_minutes(&self) -> i64 {
        self.milestones
            .iter()
            .flat_map(|m| m.steps.iter())
            .map(|s| s.minutes)
            .sum()
    }
}

/// 解析一篇长期规划文档。
///
/// 格式（Markdown + TOML frontmatter）：
///
/// ```markdown
/// ---
/// id = "rust"
/// name = "学 Rust 基础"
/// kind = "daily"
/// weight = 50.0
/// anchor = "2027-02-28"
/// ---
///
/// # 目标
///
/// 能读懂并写出安全的 Rust 代码。
///
/// ## 验收标准
///
/// - 能读懂并修复借用检查器的三类报错
///
/// # 停止条件
///
/// - 连续两周安排的整块一个都没做 → 目标选错了
///
/// ## M1 环境与基本语法
///
/// 能 cargo new 一个项目并让 cargo test 跑起来。
///
/// - [ ] 看 Book Ch1（60）
/// ```
pub fn parse_plan(markdown: &str) -> Result<LongTermPlan> {
    let (frontmatter, body) = split_frontmatter(markdown)?;
    let meta = parse_frontmatter(&frontmatter)?;

    let mut goal = String::new();
    let mut acceptance: Vec<String> = Vec::new();
    let mut stop_conditions: Vec<String> = Vec::new();
    let mut milestones: Vec<PlanMilestone> = Vec::new();
    let mut unparsed: Vec<String> = Vec::new();

    #[derive(PartialEq, Clone, Copy)]
    enum Section {
        Other,
        Goal,
        Acceptance,
        Stop,
        Milestone,
    }
    let mut section = Section::Other;
    let mut current: Option<PlanMilestone> = None;

    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(title) = line.strip_prefix("## ") {
            if let Some(m) = current.take() {
                milestones.push(m);
            }
            let title = title.trim();
            if title == "验收标准" {
                section = Section::Acceptance;
            } else if title == "停止条件" {
                section = Section::Stop;
            } else {
                section = Section::Milestone;
                current = Some(PlanMilestone {
                    name: title.to_owned(),
                    description: String::new(),
                    steps: Vec::new(),
                });
            }
            continue;
        }
        if let Some(title) = line.strip_prefix("# ") {
            if let Some(m) = current.take() {
                milestones.push(m);
            }
            let title = title.trim();
            section = match title {
                "目标" => Section::Goal,
                "验收标准" => Section::Acceptance,
                "停止条件" => Section::Stop,
                _ => Section::Other,
            };
            continue;
        }
        if let Some(item) = line.strip_prefix("- ") {
            let item = item.trim();
            match section {
                Section::Acceptance => acceptance.push(clean_item(item)),
                Section::Stop => stop_conditions.push(clean_item(item)),
                Section::Milestone => {
                    if let Some(m) = current.as_mut() {
                        if let Some((name, minutes)) = parse_step(item) {
                            m.steps.push(PlanStep { name, minutes });
                            continue;
                        }
                        // 里程碑里的 `- ` 行却解析不出时长 → 真问题，要报
                        unparsed.push(line.to_owned());
                    }
                }
                Section::Goal | Section::Other => {}
            }
            continue;
        }
        match section {
            Section::Goal => {
                if !goal.is_empty() {
                    goal.push('\n');
                }
                goal.push_str(line);
            }
            Section::Milestone => {
                if let Some(m) = current.as_mut() {
                    if !m.description.is_empty() {
                        m.description.push('\n');
                    }
                    m.description.push_str(line);
                }
            }
            Section::Acceptance | Section::Stop | Section::Other => {}
        }
    }
    if let Some(m) = current.take() {
        milestones.push(m);
    }

    Ok(LongTermPlan {
        meta,
        goal,
        acceptance,
        stop_conditions,
        milestones,
        unparsed,
    })
}

/// 把 AI 拆出来的 step 流写回文档。
///
/// 幂等：已有的 step 行会被替换成新的一组，文档其余内容（frontmatter、
/// 目标陈述、验收标准、停止条件、里程碑描述）原样保留。
///
/// `steps_by_milestone` 的键是里程碑名（必须与文档里的完全一致）。
pub fn apply_steps(
    markdown: &str,
    steps_by_milestone: &[(String, Vec<PlanStep>)],
) -> Result<String> {
    let (frontmatter, body) = split_frontmatter(markdown)?;

    let mut out = String::new();
    let mut current: Option<String> = None;
    let mut in_milestone = false;
    let mut trailing: Vec<String> = Vec::new();

    for raw in body.lines() {
        let line = raw.trim();
        if let Some(title) = line.strip_prefix("## ") {
            if let Some(name) = current.take() {
                let rendered = render_steps(&name, steps_by_milestone);
                out.push_str(&rendered);
            }
            let title = title.trim();
            // 「验收标准」「停止条件」是特殊段，不当里程碑
            in_milestone = title != "验收标准" && title != "停止条件";
            current = if in_milestone {
                Some(title.to_owned())
            } else {
                None
            };
            out.push_str(raw);
            out.push('\n');
            continue;
        }
        if line.starts_with("# ") {
            if let Some(name) = current.take() {
                let rendered = render_steps(&name, steps_by_milestone);
                out.push_str(&rendered);
            }
            current = None;
            in_milestone = false;
            out.push_str(raw);
            out.push('\n');
            continue;
        }
        if in_milestone
            && current.is_some()
            && line.starts_with("- ")
            && parse_step(line.trim_start_matches("- ")).is_some()
            && steps_by_milestone
                .iter()
                .any(|(n, _)| n == current.as_deref().unwrap_or(""))
        {
            // 旧 step 行：只删「本次要重写」的里程碑的，其余保留
            continue;
        }
        if in_milestone && current.is_some() {
            trailing.push(raw.to_owned());
            out.push_str(raw);
            out.push('\n');
            continue;
        }
        out.push_str(raw);
        out.push('\n');
    }
    if let Some(name) = current.take() {
        let rendered = render_steps(&name, steps_by_milestone);
        out.push_str(&rendered);
    }

    let _ = trailing;
    Ok(format!("---\n{frontmatter}---\n{out}"))
}

/// 渲染某个里程碑的 step 行。
fn render_steps(name: &str, steps_by_milestone: &[(String, Vec<PlanStep>)]) -> String {
    let Some((_, steps)) = steps_by_milestone.iter().find(|(n, _)| n == name) else {
        return String::new();
    };
    let mut out = String::new();
    for s in steps {
        out.push_str(&format!("- [ ] {}（{}）\n", s.name, s.minutes));
    }
    out
}

/// 拆 frontmatter：开头 `---` 到下一个 `---`。
fn split_frontmatter(markdown: &str) -> Result<(String, String)> {
    let mut lines = markdown.lines();
    let first = lines.next().ok_or_else(|| anyhow!("文档为空"))?;
    if first.trim() != "---" {
        bail!("缺少 frontmatter（应以 --- 开头）");
    }
    let mut fm = String::new();
    let mut body = String::new();
    let mut in_fm = true;
    for line in lines {
        if in_fm && line.trim() == "---" {
            in_fm = false;
            continue;
        }
        if in_fm {
            fm.push_str(line);
            fm.push('\n');
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    if in_fm {
        bail!("frontmatter 没有闭合");
    }
    Ok((fm, body))
}

/// 解析 frontmatter（TOML 风格键值）。
fn parse_frontmatter(frontmatter: &str) -> Result<PlanMeta> {
    let mut id = None;
    let mut name = None;
    let mut kind = None;
    let mut weight = None;
    let mut anchor = None;
    let mut weekly_blocks = 0usize;

    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw)) = line.split_once('=') else {
            bail!("frontmatter 行无法解析: {line}");
        };
        let key = key.trim();
        let raw = raw.trim();
        let value = raw.trim_matches('"').trim_matches('\'').trim().to_owned();
        match key {
            "id" => id = Some(value),
            "name" => name = Some(value),
            "kind" => {
                kind = Some(match value.as_str() {
                    "pulse" => PlanKind::Pulse,
                    "course" => PlanKind::Course,
                    "daily" => PlanKind::Daily,
                    other => bail!("未知 kind: {other}"),
                })
            }
            "weight" => {
                weight = Some(
                    value
                        .parse::<f64>()
                        .map_err(|e| anyhow!("weight 解析失败: {e}"))?,
                )
            }
            "anchor" => anchor = Some(value),
            "weekly_blocks" => {
                weekly_blocks = value
                    .parse::<usize>()
                    .map_err(|e| anyhow!("weekly_blocks 解析失败: {e}"))?
            }
            other => bail!("frontmatter 出现未知键: {other}"),
        }
    }

    Ok(PlanMeta {
        id: id.ok_or_else(|| anyhow!("缺少 id"))?,
        name: name.ok_or_else(|| anyhow!("缺少 name"))?,
        kind: kind.ok_or_else(|| anyhow!("缺少 kind"))?,
        weight: weight.ok_or_else(|| anyhow!("缺少 weight"))?,
        anchor,
        weekly_blocks,
    })
}

/// 去掉列表项前缀与复选框标记。
fn clean_item(item: &str) -> String {
    item.trim_start_matches("[ ] ")
        .trim_start_matches("[x] ")
        .trim()
        .to_owned()
}

/// 解析一行 step：`- [ ] 名字（60）` 或 `- 名字 (60)`
///
/// 三个坑：
/// 1. **不能用字节偏移切** —— 全角括号是 3 字节 UTF-8，按 1 字节切会 panic
///    （"not a char boundary"）。
/// 2. **括号可能嵌套** —— 「读 Book Ch1（安装 + cargo）（60）」里，往前找第一个
///    `（` 会找错到内层那个。要做**括号配对扫描**。
/// 3. **时长在括号里面** —— `名字（60）` 的 60 是闭括号**之前**的内容。
pub fn parse_step(item: &str) -> Option<(String, i64)> {
    let body = item
        .strip_prefix("[ ] ")
        .or_else(|| item.strip_prefix("[x] "))
        .unwrap_or(item)
        .trim();

    let chars: Vec<(usize, char)> = body.char_indices().collect();
    let mut depth = 0i32;
    let mut close_idx: Option<usize> = None;
    for &(i, c) in chars.iter().rev() {
        match c {
            ')' | '）' => {
                if depth == 0 {
                    close_idx = Some(i);
                }
                depth += 1;
            }
            '(' | '（' => {
                if depth == 0 {
                    continue; // 孤立的开括号
                }
                depth -= 1;
                if depth == 0 {
                    let Some(close) = close_idx else { continue };
                    let name = body[..i].trim();
                    let inner = &body[i + c.len_utf8()..close];
                    let Ok(minutes) = inner.trim().parse::<i64>() else {
                        continue;
                    };
                    if minutes <= 0 || name.is_empty() {
                        continue;
                    }
                    return Some((name.to_owned(), minutes));
                }
            }
            _ => {}
        }
    }
    None
}

/// Agent 输出的代码围栏标记（长期规划拆解协议）。
pub const DECOMPOSE_FENCE: &str = "ru41-plan-decompose";

/// 从 agent 输出里提取 step 流，并校验。
///
/// 校验项（每一条都会挡住落盘）：
/// - 围栏内必须是合法 JSON
/// - 每个里程碑的 `name` 必须在待拆解列表里（防止 agent 编造/改名）
/// - 每个里程碑都要有非空 steps
/// - 每个 step 的 minutes 必须是正整数
pub fn extract_steps(summary: &str, expected: &[String]) -> Result<Vec<(String, Vec<PlanStep>)>> {
    let json = extract_fenced_json(summary)
        .ok_or_else(|| anyhow!("输出里没有找到 ```{DECOMPOSE_FENCE}``` 代码块"))?;

    #[derive(serde::Deserialize)]
    struct Payload {
        milestones: Vec<MilestoneSteps>,
    }
    #[derive(serde::Deserialize)]
    struct MilestoneSteps {
        name: String,
        steps: Vec<StepIn>,
    }
    #[derive(serde::Deserialize)]
    struct StepIn {
        name: String,
        minutes: i64,
    }

    let payload: Payload =
        serde_json::from_str(&json).map_err(|e| anyhow!("JSON 解析失败: {e}"))?;

    if payload.milestones.is_empty() {
        bail!("输出里没有任何里程碑");
    }

    let mut out: Vec<(String, Vec<PlanStep>)> = Vec::new();
    for m in payload.milestones {
        if !expected.iter().any(|e| e == &m.name) {
            bail!("出现未知的里程碑「{}」", m.name);
        }
        if out.iter().any(|(n, _)| n == &m.name) {
            bail!("里程碑「{}」重复", m.name);
        }
        if m.steps.is_empty() {
            bail!("里程碑「{}」没有 steps", m.name);
        }
        let mut steps = Vec::new();
        for s in m.steps {
            if s.minutes <= 0 {
                bail!("里程碑「{}」的 step「{}」时长非法", m.name, s.name);
            }
            if s.name.trim().is_empty() {
                bail!("里程碑「{}」有空的 step 名", m.name);
            }
            steps.push(PlanStep {
                name: s.name.trim().to_owned(),
                minutes: s.minutes,
            });
        }
        out.push((m.name, steps));
    }

    // 待拆解的里程碑都要被覆盖
    let missing: Vec<&String> = expected
        .iter()
        .filter(|e| !out.iter().any(|(n, _)| n == *e))
        .collect();
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
        bail!("这些里程碑没有拆解：{}", names.join("、"));
    }

    Ok(out)
}

/// 提取围栏内的 JSON 文本；没有围栏时整段按 JSON 试。
fn extract_fenced_json(summary: &str) -> Option<String> {
    let marker = format!("```{DECOMPOSE_FENCE}");
    if let Some(start) = summary.find(&marker) {
        let rest = &summary[start + marker.len()..];
        let end = rest.find("```")?;
        return Some(rest[..end].trim().to_owned());
    }
    let trimmed = summary.trim();
    if trimmed.starts_with('{') {
        return Some(trimmed.to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"---
id = "rust"
name = "学 Rust 基础"
kind = "daily"
weight = 50.0
---

# 目标

能读懂并写出安全的 Rust 代码。

## 验收标准

- 能读懂并修复借用检查器的三类报错
- 能从零写出一个带测试的 CLI 工具

# 停止条件

- 连续两周安排的整块一个都没做 → 目标选错了

## M1 环境与基本语法

能 cargo new 一个项目并让 cargo test 跑起来。

- [ ] 看 Book Ch1（60）

## M2 所有权与借用

借用规则和借用检查器的三类报错。
"#;

    #[test]
    fn parses_meta_and_sections() {
        let plan = parse_plan(DOC).expect("应能解析");
        assert_eq!(plan.meta.id, "rust");
        assert_eq!(plan.meta.name, "学 Rust 基础");
        assert_eq!(plan.meta.kind, PlanKind::Daily);
        assert_eq!(plan.meta.weight, 50.0);
        assert_eq!(plan.goal, "能读懂并写出安全的 Rust 代码。");
        assert_eq!(plan.acceptance.len(), 2);
        assert_eq!(plan.stop_conditions.len(), 1);
        assert_eq!(plan.milestones.len(), 2);
        assert_eq!(plan.milestones[0].steps.len(), 1);
        assert_eq!(plan.milestones[0].steps[0].minutes, 60);
    }

    #[test]
    fn keeps_milestone_description() {
        let plan = parse_plan(DOC).expect("应能解析");
        assert!(plan.milestones[0].description.contains("cargo new"));
        assert!(plan.milestones[1].description.contains("借用检查器"));
    }

    #[test]
    fn reports_milestones_missing_steps() {
        let plan = parse_plan(DOC).expect("应能解析");
        assert_eq!(plan.needs_llm_steps(), vec!["M2 所有权与借用".to_owned()]);
    }

    #[test]
    fn rejects_missing_frontmatter() {
        assert!(parse_plan("# 没有 frontmatter\n").is_err());
    }

    #[test]
    fn rejects_unknown_frontmatter_key() {
        let doc = "---\nid = \"x\"\nname = \"y\"\nkind = \"daily\"\nweight = 1.0\nfoo = 1\n---\n";
        assert!(parse_plan(doc).is_err());
    }

    #[test]
    fn parses_step_with_nested_parentheses() {
        let (name, minutes) = parse_step("读 Book Ch1（安装 + cargo）（60）").expect("应能解析");
        assert_eq!(minutes, 60);
        assert_eq!(name, "读 Book Ch1（安装 + cargo）");
    }

    #[test]
    fn apply_steps_is_idempotent() {
        let steps = vec![(
            "M1 环境与基本语法".to_owned(),
            vec![
                PlanStep {
                    name: "读 Book Ch1".to_owned(),
                    minutes: 60,
                },
                PlanStep {
                    name: "读 Book Ch3".to_owned(),
                    minutes: 90,
                },
            ],
        )];
        let once = apply_steps(DOC, &steps).expect("应能写回");
        assert!(once.contains("- [ ] 读 Book Ch1（60）"));
        assert!(once.contains("- [ ] 读 Book Ch3（90）"));
        // 原文的 step 行被替换而不是重复
        assert_eq!(once.matches("读 Book Ch1").count(), 1);
        assert_eq!(
            once.matches("看 Book Ch1").count(),
            0,
            "旧 step 行应被替换掉"
        );

        let twice = apply_steps(&once, &steps).expect("应能写回");
        assert_eq!(once, twice, "重复写回应得到同一结果");
    }

    #[test]
    fn apply_steps_preserves_other_sections() {
        let steps = vec![(
            "M1 环境与基本语法".to_owned(),
            vec![PlanStep {
                name: "读 Book Ch1".to_owned(),
                minutes: 60,
            }],
        )];
        let out = apply_steps(DOC, &steps).expect("应能写回");
        assert!(out.contains("# 目标"));
        assert!(out.contains("能读懂并写出安全的 Rust 代码。"));
        assert!(out.contains("- 能读懂并修复借用检查器的三类报错"));
        assert!(out.contains("能 cargo new 一个项目并让 cargo test 跑起来。"));
    }
}
