//! 端到端：人写的长期规划文档 → agent 拆解输出 → 写回 → 再解析。
//!
//! 用真实的 `docs/plans/rust-baseline-plan.md`（战略层，人写的、没有 step）
//! 和一份符合 skill 规则的 agent 输出，跑完整条链路，验证：
//!
//! 1. 人写的文档解析出待拆解里程碑
//! 2. agent 输出被校验接受（名字对得上、时长为正、里程碑全覆盖）
//! 3. 写回后文档里有了 step，且**其它内容一字未动**（目标/验收/停止条件/描述）
//! 4. 写回是幂等的（再写一次结果相同）
//!
//! 跑法：`cargo test --release --test plan_roundtrip`

use std::path::PathBuf;

use ru41::domain::plan::{self, LongTermPlan, PlanStep};

/// agent 按 `plan-decompose` skill 规则产出的输出（真实样本）。
const AGENT_OUTPUT: &str = r#"```ru41-plan-decompose
{"milestones":[
 {"name":"M1 环境与基本语法","steps":[
   {"name":"读 Book Ch1（安装 + cargo + hello world）","minutes":60},
   {"name":"读 Book Ch3.1-3.2（变量与类型）","minutes":90},
   {"name":"读 Book Ch3.3-3.5（函数与控制流）","minutes":60},
   {"name":"动手：shadowing / if-match 循环练习","minutes":90}]},
 {"name":"M2 所有权与借用","steps":[
   {"name":"读 Book Ch4.1（所有权三规则 + move）","minutes":90},
   {"name":"读 Book Ch4.2（引用与借用）","minutes":120},
   {"name":"动手：借用检查器三类错误实操","minutes":120},
   {"name":"动手：切片 + struct 练习","minutes":90}]},
 {"name":"M3 类型系统","steps":[
   {"name":"读 Book Ch5-6（struct / enum / match）","minutes":90},
   {"name":"动手：Option 与 Result 练习","minutes":90},
   {"name":"读 Book Ch9（错误处理 + ? 运算符）","minutes":90},
   {"name":"动手：写一个带自定义错误的模块","minutes":60}]},
 {"name":"M4 泛型与生命周期","steps":[
   {"name":"读 Book Ch10.1-10.2（泛型 / trait）","minutes":90},
   {"name":"读 Book Ch10.3（生命周期）","minutes":90},
   {"name":"动手：trait + 泛型约束练习","minutes":60},
   {"name":"动手：修 5 个生命周期标注错误","minutes":90}]},
 {"name":"M5 动手写一个 CLI 工具","steps":[
   {"name":"读 Book Ch12（minigrep 全流程）","minutes":90},
   {"name":"实现 minigrep 主体","minutes":180},
   {"name":"补测试 + 错误处理","minutes":90},
   {"name":"动手：给 Rust 计划补 12 个示例的完整测试","minutes":180}]}
]}
```"#;

fn real_plan_doc() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/plans/rust-baseline-plan.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读 {} 失败: {e}", path.display()))
}

/// 从 agent 输出里提取 step 流（与插件同款校验）。
fn extract(summary: &str, expected: &[String]) -> Vec<(String, Vec<PlanStep>)> {
    let marker = "```ru41-plan-decompose";
    let start = summary.find(marker).expect("应有围栏");
    let rest = &summary[start + marker.len()..];
    let end = rest.find("```").expect("围栏应闭合");
    let json = rest[..end].trim();

    #[derive(serde::Deserialize)]
    struct Payload {
        milestones: Vec<M>,
    }
    #[derive(serde::Deserialize)]
    struct M {
        name: String,
        steps: Vec<S>,
    }
    #[derive(serde::Deserialize)]
    struct S {
        name: String,
        minutes: i64,
    }
    let payload: Payload = serde_json::from_str(json).expect("JSON 应合法");

    for m in &payload.milestones {
        assert!(
            expected.iter().any(|e| e == &m.name),
            "出现未知里程碑 {}",
            m.name
        );
        assert!(!m.steps.is_empty(), "{} 的 steps 为空", m.name);
        for s in &m.steps {
            assert!(s.minutes > 0, "{} 的 step 时长非法", m.name);
        }
    }
    for e in expected {
        assert!(
            payload.milestones.iter().any(|m| &m.name == e),
            "里程碑 {e} 没被拆解"
        );
    }
    payload
        .milestones
        .into_iter()
        .map(|m| {
            (
                m.name,
                m.steps
                    .into_iter()
                    .map(|s| PlanStep {
                        name: s.name,
                        minutes: s.minutes,
                    })
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn human_plan_to_agent_output_to_written_back_plan() {
    let original = real_plan_doc();

    // 1) 人写的战略文档：能解析，且五个里程碑都待拆解
    let plan: LongTermPlan = plan::parse_plan(&original).expect("应能解析");
    assert_eq!(plan.meta.id, "rust");
    assert_eq!(plan.meta.name, "学 Rust 基础");
    assert!(!plan.goal.is_empty(), "目标陈述应被解析");
    assert_eq!(plan.acceptance.len(), 5, "五条验收标准");
    assert_eq!(plan.stop_conditions.len(), 3, "三条停止条件");
    let pending = plan.needs_llm_steps();
    assert_eq!(pending.len(), 5, "五个里程碑都还没拆");
    assert!(
        !plan.milestones[0].description.is_empty(),
        "里程碑描述应被解析"
    );

    // 2) agent 输出被接受
    let steps = extract(AGENT_OUTPUT, &pending);

    // 3) 写回
    let updated = plan::apply_steps(&original, &steps).expect("应能写回");

    let reparsed = plan::parse_plan(&updated).expect("写回后应能再解析");
    assert!(
        reparsed.needs_llm_steps().is_empty(),
        "写回后不应再有待拆解里程碑"
    );
    assert_eq!(
        reparsed.total_minutes(),
        1920,
        "总时长 32 小时（M1-M5 实际之和）"
    );
    assert_eq!(reparsed.acceptance.len(), 5, "验收标准不该被改");
    assert_eq!(reparsed.stop_conditions.len(), 3, "停止条件不该被改");
    assert_eq!(reparsed.goal, plan.goal, "目标陈述不该被改");
    assert_eq!(
        reparsed.milestones[0].description, plan.milestones[0].description,
        "里程碑描述不该被改"
    );
    assert_eq!(reparsed.meta.weight, 50.0, "权重不该被改");

    // 4) 幂等
    let again = plan::apply_steps(&updated, &steps).expect("应能再写回");
    assert_eq!(updated, again, "重复写回应得到同一结果");

    // 5) 步骤确实落进文档了
    assert!(updated.contains("- [ ] 读 Book Ch1（安装 + cargo + hello world）（60）"));
    assert!(updated.contains("- [ ] 实现 minigrep 主体（180）"));
}
