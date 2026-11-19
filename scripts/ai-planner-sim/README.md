# ai-planner-sim · 多目标 AI 规划模拟

跑一遍完整的「AI 规划、人类学习」：L0 硬锚定倒排 → L1 跟课绑定 → 碎片 FSRS 复习调度 →
剩余整块跨目标背包分配，并输出**可行性诊断**与**取舍建议**。

```bash
cd scripts/ai-planner-sim
cp config.example.toml config.toml   # config.toml 不入库（含真实锚点）
cargo run --release
```

依赖 `fsrs = "7.0.0"`（官方 Rust 实现，BSD-3-Clause）、`serde`、`toml`、`anyhow`。

## 三种运行模式

```bash
cargo run --release                          # 跑一遍规划
cargo run --release -- --config my.toml      # 换配置
cargo run --release -- --sweep cet4          # 扫「四级提前多少周开始」
cargo run --release -- --combinations        # 枚举目标子集：这学期能同时推进哪几个
cargo test --release                        # 7 个回归测试（守住分配顺序等不变量）
```

## 研究日志

结论与后续实验见 [docs/plans/ai-planner-research.md](../../docs/plans/ai-planner-research.md)。

## 时间预算（来自生产真实课表）

- **整块**：周一晚 19:00-21:00 + 周日上午 10:00-12:00，各 2 小时 → 学期内 13 块
- **碎片**：课间缝 90 分钟/周（周一 10 + 周三 30 + 周四 40 + 周五 10）

## ⚠️ 锚点是假设值

四级 12-19 / HICP 12-31 / 雅思 2027-02-20 / 高数期末 01-15 —— **演示用**，
真实值见 [portfolio.md §8](../../docs/plans/portfolio.md) 的待补清单。
目标权重（80/70/75/60/50）也是示例值。

## 输出

1. 逐周排期（哪天、哪个槽位、哪个目标、做什么）
2. 期末各目标进度
3. 冲突告警（每周需求 vs 可用）
4. **可行性诊断**（总需求 vs 总容量、硬锚定是否自冲突）
5. **取舍建议**（暂缓哪个目标性价比最高）
6. 碎片复习统计（真 FSRS v7：复习次数、留存率）

背景见 [docs/plans/ai-planner.md](../../docs/plans/ai-planner.md)。
