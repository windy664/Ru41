//! 长期规划文档 —— 战略层产物。
//!
//! ## 它在整条链路里的位置
//!
//! ```text
//!   人给方向（目标 / 权重 / 锚点）
//!        ↓
//!   [长期规划文档]  ← 人审过，存在 Vault 的 08-Planning-规划/Plans-计划/
//!        ↓  AI 拆解（agent 任务）
//!   带 step 的计划  ← 名字 + 时长
//!        ↓  确定性装箱（planner.rs）
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
//! - **人必须给**：`id` / `name` / `kind` / `weight` / `anchor` / 目标陈述 / 验收标准
//! - **AI 负责补**：每个里程碑下的 `steps`（名字 + 时长）
//! - **人审**：AI 补的 steps 要写回文档，人看过才算数
//!
//! 解析器会检查"该有的有没有"，缺了就报出来 —— 缺的东西就是 AI 该干的。

use crate::config::{GoalKind, GoalSpec, MilestoneSpec, TaskStep};

/// 一篇长期规划文档的解析结果。
#[derive(Debug, Clone)]
pub struct LongTermPlan {
    pub spec: GoalSpec,
    /// 每个里程碑的验收标准（人写的，进算法没用，但要有 —— 否则无法验收）
    pub acceptance: Vec<String>,
    /// 缺 steps 的里程碑名 —— 这些需要 AI 补
    pub needs_llm_steps: Vec<String>,
    /// 文档里写了但没解析出来的行（防止静默丢内容）
    pub unparsed: Vec<String>,
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
/// 能读懂并写出安全的 Rust 代码，能独立写一个带测试的 CLI 工具。
///
/// ## 验收标准
/// - 能读懂并修复借用检查器的三类报错
/// - 能从零写出一个带测试的 CLI 工具
///
/// ## M1 环境与基本语法
/// - [ ] 看 Book Ch1（60）
/// - [ ] 看 Book Ch3.1-3.2 变量与类型（90）
///
/// ## M2 所有权与借用
/// - [ ] 看 Book Ch4.1 所有权三规则（60）
/// ```
pub fn parse_plan(markdown: &str) -> Result<LongTermPlan, String> {
    let (frontmatter, body) = split_frontmatter(markdown)?;
    let meta: toml::Value =
        toml::from_str(&frontmatter).map_err(|e| format!("frontmatter 解析失败: {e}"))?;

    let id = meta
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("缺少 id")?
        .to_owned();
    let name = meta
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("缺少 name")?
        .to_owned();
    let kind = match meta.get("kind").and_then(|v| v.as_str()) {
        Some("pulse") => GoalKind::Pulse,
        Some("course") => GoalKind::Course,
        Some("daily") => GoalKind::Daily,
        Some(other) => return Err(format!("未知 kind: {other}")),
        None => return Err("缺少 kind".into()),
    };
    let weight = meta
        .get("weight")
        .and_then(|v| v.as_float())
        .ok_or("缺少 weight")?;
    let anchor = meta
        .get("anchor")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());
    let weekly_blocks = meta
        .get("weekly_blocks")
        .and_then(|v| v.as_integer())
        .unwrap_or(0)
        .max(0) as usize;

    let mut milestones: Vec<MilestoneSpec> = Vec::new();
    let mut acceptance: Vec<String> = Vec::new();
    let mut needs_llm_steps: Vec<String> = Vec::new();
    let mut unparsed: Vec<String> = Vec::new();

    let mut current: Option<MilestoneSpec> = None;
    // 当前所在的段：一级标题段是「目标 / 验收标准 / 停止条件 / 配套材料」这类说明，
    // 二级标题段是里程碑。只有里程碑段里的 `- ` 行才可能是 step。
    #[derive(PartialEq)]
    enum Section {
        Other,
        Acceptance,
        Milestone,
    }
    let mut section = Section::Other;

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
            } else {
                section = Section::Milestone;
                current = Some(MilestoneSpec {
                    name: title.to_owned(),
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
            if title == "验收标准" {
                section = Section::Acceptance;
                section = Section::Acceptance;
            } else {
                section = Section::Other;
                section = Section::Milestone;
            }
            continue;
        }
        if let Some(item) = line.strip_prefix("- ") {
            let item = item.trim();
            match section {
                Section::Acceptance => {
                    acceptance.push(item.trim_start_matches("[ ] ").trim().to_owned());
                }
                Section::Milestone => {
                    if let Some(m) = current.as_mut() {
                        if let Some((name, minutes)) = parse_step(item) {
                            m.steps.push(TaskStep { name, minutes });
                            continue;
                        }
                        // 里程碑里的 `- ` 行却解析不出时长 → 真问题，要报
                        unparsed.push(line.to_owned());
                    }
                }
                Section::Other => { /* 说明段的列表项，忽略 */ }
            }
            continue;
        }
        // 普通段落：说明性文字，忽略
    }
    if let Some(m) = current.take() {
        milestones.push(m);
    }

    for m in &milestones {
        if m.steps.is_empty() {
            needs_llm_steps.push(m.name.clone());
        }
    }

    Ok(LongTermPlan {
        spec: GoalSpec {
            id,
            name,
            kind,
            weight,
            anchor,
            weekly_blocks,
            milestones,
        },
        acceptance,
        needs_llm_steps,
        unparsed,
    })
}

/// 拆 frontmatter：开头 `---` 到下一个 `---`。
fn split_frontmatter(markdown: &str) -> Result<(String, String), String> {
    let mut lines = markdown.lines();
    let first = lines.next().ok_or("文档为空")?;
    if first.trim() != "---" {
        return Err("缺少 frontmatter（应以 --- 开头）".into());
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
        return Err("frontmatter 没有闭合".into());
    }
    Ok((fm, body))
}

/// 解析一行 step：`- [ ] 名字（60）` 或 `- 名字 (60)`
///
/// 三个坑：
/// 1. **不能用字节偏移切** —— 全角括号是 3 字节 UTF-8，按 1 字节切会 panic
///    （"not a char boundary"）。
/// 2. **括号可能嵌套** —— 「读 Book Ch1（安装 + cargo）（60）」里，往前找第一个
///    `（` 会找错到内层那个。要做**括号配对扫描**。
/// 3. **时长在括号里面** —— `名字（60）` 的 60 是闭括号**之前**的内容。
fn parse_step(item: &str) -> Option<(String, i64)> {
    let body = item
        .strip_prefix("[ ] ")
        .or_else(|| item.strip_prefix("[x] "))
        .unwrap_or(item)
        .trim();

    // 从末尾往前扫，维护嵌套深度：depth 0→1 时记下最外层闭括号，
    // depth 1→0 时那个开括号就是与它配对的那个。
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

## M1 环境与基本语法

- [ ] 看 Book Ch1（60）
- [ ] 看 Book Ch3.1-3.2 变量与类型（90）

## M2 所有权与借用

- [ ] 看 Book Ch4.1 所有权三规则（60）
"#;

    #[test]
    fn parses_frontmatter_and_milestones() {
        let plan = parse_plan(DOC).expect("应能解析");
        assert_eq!(plan.spec.id, "rust");
        assert_eq!(plan.spec.name, "学 Rust 基础");
        assert_eq!(plan.spec.kind, GoalKind::Daily);
        assert_eq!(plan.spec.weight, 50.0);
        assert_eq!(plan.spec.milestones.len(), 2);
        assert_eq!(plan.spec.milestones[0].steps.len(), 2);
        assert_eq!(plan.spec.milestones[0].steps[0].minutes, 60);
        assert_eq!(plan.spec.milestones[0].steps[1].name, "看 Book Ch3.1-3.2 变量与类型");
    }

    #[test]
    fn collects_acceptance_criteria() {
        let plan = parse_plan(DOC).expect("应能解析");
        assert_eq!(plan.acceptance.len(), 2);
        assert!(plan.acceptance[0].contains("借用检查器"));
    }

    #[test]
    fn reports_milestones_missing_steps() {
        // 删掉 M1 的 steps → 需要 LLM 补
        // 删掉 M1 的全部 step 行 → M1 需要 AI 补
        let doc = DOC
            .replace("- [ ] 看 Book Ch1（60）\n", "")
            .replace("- [ ] 看 Book Ch3.1-3.2 变量与类型（90）\n", "");
        let plan = parse_plan(&doc).expect("应能解析");
        assert_eq!(plan.needs_llm_steps, vec!["M1 环境与基本语法".to_owned()]);
        assert!(plan.needs_llm_steps.iter().all(|n| n != "M2 所有权与借用"));
    }

    #[test]
    fn parses_step_with_nested_parentheses() {
        // 名字里带括号，不能被当成时长括号
        let (name, minutes) = parse_step("读 Book Ch1（安装 + cargo）（60）").expect("应能解析");
        assert_eq!(minutes, 60);
        assert_eq!(name, "读 Book Ch1（安装 + cargo）");
    }

    #[test]
    fn rejects_missing_frontmatter() {
        assert!(parse_plan("# 没有 frontmatter\n").is_err());
    }

    #[test]
    fn rejects_missing_required_fields() {
        let doc = "---\nid = \"x\"\n---\n\n# 目标\n";
        assert!(parse_plan(doc).is_err(), "缺 name/kind/weight 应报错");
    }
}
