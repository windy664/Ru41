# fsrs-probe · 碎片池复习负载模拟

用**真实 FSRS v7 算法**算 Ru41 的碎片池（课表实测：90 分钟/周）能承载多少知识点。

```bash
cd scripts/fsrs-probe
cargo run --release
```

需要能访问 crates.io（`index.crates.io` + `static.crates.io`）。依赖 `fsrs = "7.0.0"`（官方 Rust 实现，BSD-3-Clause）。

场景：
1. 碎片预算充足 —— Naive vs FSRS 是否有差别
2. 碎片预算吃紧 —— 差别是否出现
3. 预算扫掠 —— 每天几分钟够用
4. 整块引入新卡速度的影响
5. 目标留存率的代价（85%/90%/95%/98%）
6. 退步信号 —— 哪些知识点该用整块重学

背景见 [docs/plans/learning-scheduling.md](../../docs/plans/learning-scheduling.md)。
