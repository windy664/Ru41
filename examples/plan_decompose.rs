//! 端到端拆解 CLI：人写的长期规划文档 + agent 输出 → 写回后的文档。
//!
//! 这是「程序落盘」那条路径的命令行版本：agent 只出 JSON，写回由本程序执行。
//!
//! ```bash
//! cargo run --release --example plan_decompose -- \
//!     docs/plans/rust-baseline-plan.md agent-output.md \
//!     -o /tmp/updated-plan.md
//! ```
//!
//! 参数：
//!   <plan>        人写的长期规划文档（Markdown + TOML frontmatter）
//!   <agent-out>   agent 的输出（含 ```ru41-plan-decompose 围栏）
//!   -o <path>     写回后的文档输出路径（缺省打到 stdout）

use std::path::PathBuf;

use ru41::domain::plan;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let plan_path = PathBuf::from(
        args.next()
            .expect("用法: plan_decompose <plan> <agent-out> [-o out]"),
    );
    let agent_path = PathBuf::from(
        args.next()
            .expect("用法: plan_decompose <plan> <agent-out> [-o out]"),
    );
    let mut out_path: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        if a == "-o" {
            out_path = Some(PathBuf::from(args.next().expect("-o 需要路径")));
        }
    }

    let content = std::fs::read_to_string(&plan_path)?;
    let plan = plan::parse_plan(&content)?;
    let expected = plan.needs_llm_steps();
    if expected.is_empty() {
        anyhow::bail!("这份规划没有待拆解的里程碑");
    }
    eprintln!(
        "目标：{}｜待拆解 {} 个里程碑",
        plan.meta.name,
        expected.len()
    );

    let summary = std::fs::read_to_string(&agent_path)?;
    let steps = ru41::domain::plan::extract_steps(&summary, &expected)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let total: i64 = steps
        .iter()
        .map(|(_, s)| s.iter().map(|x| x.minutes).sum::<i64>())
        .sum();
    eprintln!("拆解通过：{} 个里程碑，合计 {} 分钟", steps.len(), total);

    let updated = plan::apply_steps(&content, &steps)?;
    match out_path {
        Some(p) => {
            std::fs::write(&p, &updated)?;
            eprintln!("已写入 {}", p.display());
        }
        None => print!("{updated}"),
    }
    Ok(())
}
