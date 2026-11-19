//! 长期规划拆解闭环：战略层文档 → AI 拆出带时长的 step 流 → 写回文档。
//!
//! ## 快照协议（与 obsidian-organize 同一套安全模型）
//!
//! Agent **没有文件系统与 git 权限**：文档正文由程序内联进 prompt（脱敏、可能截断），
//! agent 只输出结构化 JSON，**落盘由程序执行**。
//!
//! ## 数据流
//!
//! ```text
//!   扫描 Vault 规划目录 → 找出「有里程碑但没 step」的文档
//!        ↓  快照：目标 / 验收标准 / 停止条件 / 各里程碑描述 内联进 prompt
//!   入队 agent 任务（幂等键 plan:decompose:{文档}:{sha}）
//!        ↓  agent 输出 ```ru41-plan-decompose JSON
//!   提取 → 校验 → apply_steps 写回文档 → 提交推送 → 通知
//! ```
//!
//! ## 为什么要有「长期规划文档」这一层
//!
//! 短期任务拆解的输入是**长期规划文档**，不是待办清单。
//! 没有文档就没有短期任务 —— 这样拆出来的任务才有出处，才能回答「这一步为什么做」。

use ru41::domain::plan::{self, LongTermPlan};

/// 幂等键：同一份文档的同一版本只拆一次。
pub fn decompose_request_key(doc: &str, sha: &str) -> String {
    format!("plan:decompose:{doc}:{sha}")
}

/// 构造快照 prompt：把「人写的」内容内联给 agent，agent 据此产出 step 流。
///
/// 只内联 agent 真正需要的：目标陈述、验收标准、停止条件、待拆解里程碑的标题与描述。
/// **不内联已有 step**（那些是上一轮产物，重拆时会被整体替换）。
pub fn decompose_prompt(plan: &LongTermPlan) -> String {
    let mut out = String::new();
    out.push_str("# 长期规划拆解任务\n\n");
    out.push_str("你要把一份长期规划里的每个里程碑，拆成**可执行、有时长的子任务序列**。\n\n");

    out.push_str(&format!("## 目标\n\n{}\n\n", plan.goal.trim()));
    out.push_str(&format!(
        "目标名：{}｜类型：{}｜权重：{}｜锚点：{}\n\n",
        plan.meta.name,
        plan.meta.kind.as_str(),
        plan.meta.weight,
        plan.meta.anchor.clone().unwrap_or_else(|| "无".into())
    ));

    if !plan.acceptance.is_empty() {
        out.push_str("## 验收标准（人定的，决定「做完没有」）\n\n");
        for a in &plan.acceptance {
            out.push_str(&format!("- {a}\n"));
        }
        out.push('\n');
    }
    if !plan.stop_conditions.is_empty() {
        out.push_str("## 停止条件（人定的，决定「该不该放弃」）\n\n");
        for s in &plan.stop_conditions {
            out.push_str(&format!("- {s}\n"));
        }
        out.push('\n');
    }

    let pending = plan.needs_llm_steps();
    out.push_str("## 待拆解的里程碑\n\n");
    for name in &pending {
        let milestone = plan
            .milestones
            .iter()
            .find(|m| &m.name == name)
            .expect("待拆解名来自 milestones");
        out.push_str(&format!("【{name}】\n"));
        let desc = milestone.description.trim();
        if desc.is_empty() {
            out.push_str("（无描述，按里程碑名判断该做什么）\n");
        } else {
            out.push_str(desc);
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("## 拆解规则\n\n");
    out.push_str("1. **每个里程碑都要给 steps**，`name` 与上面【】里的完全一致，不要改写、不要新增。\n");
    out.push_str("2. `minutes` 是**这一步的净时长**（分钟，正整数）。估「一个人能独立做完这一步」要多久，\n");
    out.push_str("   不是这一阶段的总量。读一章书、写一个练习、跑一次复习，各自估。\n");
    out.push_str("3. `steps` 是**有序**的，按实际执行顺序排；一个里程碑通常 3-6 步。\n");
    out.push_str("4. 只拆解上面列出的里程碑。文档里已有的里程碑但没在「待拆解」列表里的，不要动。\n");
    out.push_str("5. 不要输出任何解释文字，只输出 JSON。\n");
    out.push_str("6. 拿不准时长时给一个保守估计；总时长过长会让规划不可行，宁可诚实估高。\n");

    out.push_str("\n## 输出（严格遵守）\n\n回复**只包含一个** ```");
    out.push_str(plan::DECOMPOSE_FENCE);
    out.push_str(" 代码块，内含如下 JSON，不要输出其它文字：\n\n```");
    out.push_str(plan::DECOMPOSE_FENCE);
    out.push_str("\n{\"milestones\":[{\"name\":\"【上面给的里程碑名】\",\"steps\":[{\"name\":\"这一步做什么\",\"minutes\":60}]}]}\n```");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan() -> LongTermPlan {
        let doc = r#"---
id = "rust"
name = "学 Rust 基础"
kind = "daily"
weight = 50.0
---

# 目标

能读懂并写出安全的 Rust 代码。

## 验收标准

- 能读懂并修复借用检查器的三类报错

# 停止条件

- 连续两周一个块都没做 → 目标选错了

## M1 环境与基本语法

能 cargo new 一个项目并让 cargo test 跑起来。

## M2 所有权与借用

借用规则和三类报错。
"#;
        ru41::domain::plan::parse_plan(doc).expect("应能解析")
    }

    #[test]
    fn prompt_contains_only_what_agent_needs() {
        let plan = sample_plan();
        let prompt = decompose_prompt(&plan);
        assert!(prompt.contains("能 cargo new 一个项目"));
        assert!(prompt.contains("能读懂并修复借用检查器的三类报错"));
        assert!(prompt.contains("连续两周一个块都没做"));
        // 待拆解的两个里程碑都要在 prompt 里
        assert!(prompt.contains("【M1 环境与基本语法】"));
        assert!(prompt.contains("【M2 所有权与借用】"));
        // 输出格式约束
        assert!(prompt.contains("ru41-plan-decompose"));
        // 已有 step 不该出现在 prompt 里（这里本来就没有）
        assert!(!prompt.contains("- [ ]"));
    }

    #[test]
    fn request_key_is_stable_and_distinguishes_sha() {
        assert_eq!(
            decompose_request_key("08-Planning-规划/Plans-计划/a.md", "abc"),
            "plan:decompose:08-Planning-规划/Plans-计划/a.md:abc"
        );
        assert_ne!(
            decompose_request_key("a.md", "abc"),
            decompose_request_key("a.md", "def")
        );
    }

    #[test]
    fn extracts_valid_steps() {
        let summary = r#"拆解如下：
```ru41-plan-decompose
{"milestones":[
 {"name":"M1 环境与基本语法","steps":[{"name":"读 Book Ch1","minutes":60},{"name":"动手练习","minutes":90}]},
 {"name":"M2 所有权与借用","steps":[{"name":"读 Book Ch4","minutes":120}]}
]}
```"#;
        let expected = vec!["M1 环境与基本语法".to_owned(), "M2 所有权与借用".to_owned()];
        let steps = plan::extract_steps(summary, &expected).expect("应能提取");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].1.len(), 2);
        assert_eq!(steps[0].1[1].minutes, 90);
        assert_eq!(steps[1].1[0].minutes, 120);
    }

    #[test]
    fn rejects_unknown_milestone() {
        let summary = r#"```ru41-plan-decompose
{"milestones":[{"name":"编造的里程碑","steps":[{"name":"x","minutes":60}]}]}
```"#;
        let expected = vec!["M1 环境与基本语法".to_owned()];
        let err = plan::extract_steps(summary, &expected).expect_err("应拒绝");
        assert!(err.to_string().contains("未知"), "实际: {err}");
    }

    #[test]
    fn rejects_missing_milestone() {
        let summary = r#"```ru41-plan-decompose
{"milestones":[{"name":"M1 环境与基本语法","steps":[{"name":"x","minutes":60}]}]}
```"#;
        let expected = vec!["M1 环境与基本语法".to_owned(), "M2 所有权与借用".to_owned()];
        let err = plan::extract_steps(summary, &expected).expect_err("应拒绝");
        assert!(err.to_string().contains("没有拆解"), "实际: {err}");
    }

    #[test]
    fn rejects_bad_minutes() {
        let summary = r#"```ru41-plan-decompose
{"milestones":[{"name":"M1 环境与基本语法","steps":[{"name":"x","minutes":0}]}]}
```"#;
        let expected = vec!["M1 环境与基本语法".to_owned()];
        assert!(plan::extract_steps(summary, &expected).is_err());
    }

    #[test]
    fn rejects_missing_fence() {
        assert!(plan::extract_steps("随便说点什么", &[]).is_err());
    }

    #[test]
    fn rejects_empty_steps() {
        let summary = r#"```ru41-plan-decompose
{"milestones":[{"name":"M1 环境与基本语法","steps":[]}]}
```"#;
        let expected = vec!["M1 环境与基本语法".to_owned()];
        assert!(plan::extract_steps(summary, &expected).is_err());
    }
}
