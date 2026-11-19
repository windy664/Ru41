---
type: plan
status: active
language: Rust
agent: mavis
updated: 2026-11-19
tags: [Ru41, planning, rust, learning]
---

# 「学 Rust 基础」学习规划（模拟版）

> 这份规划是 [longterm-planning.md](../longterm-planning.md) §10 那次模拟的**具体产物**：不是抽象里程碑，而是"哪一天、读哪一章、做什么练习、产出什么、怎么验收"。

## 0. 目标与边界

| | |
|---|---|
| **目标** | Rust 基础：能读懂并写出安全的 Rust 代码，能独立写一个带测试的 CLI 工具 |
| **不追求** | 完整 Rust 语言覆盖、宏、unsafe、异步、并发 —— 那些放寒假 |
| **周期** | 学期内 6.5 周（第 12-18 周）+ 寒假 8 周 |
| **容量** | 学期内 26h（13 块 × 2h）／寒假 64h（8 周 × 2×4h） |
| **节奏** | 周一晚 19:00-21:00 + 周日上午 10:00-12:00（系统按可用性+间隔推荐，不是人工指定） |

**为什么不是"完整 Rust"**：完整路线约 90h，学期内容量只有 26h —— 缺 64h。跨学期才够（26 + 64 = 90h）。详见 §4。

## 1. 学期内 13 块（真实课表排出来的）

排期由 `scripts/plan-sim.py` 从生产课表算出，槽位推荐逻辑见 longterm-planning §10.2–10.4。

| # | 日期 | 周 | 内容 | Rust Book | 练习 | 验收产出 |
|---|---|---|---|---|---|---|
| 1 | 11-22 | 日 | 环境与基本语法 | Ch1 + Ch3.1-3.5 | cargo new / cargo test；shadowing、if-match、loop 返回值 | 能独立建项目并让测试跑起来 |
| 2 | 11-23 | 一 | 所有权（上） | Ch4.1-4.2 | 移动语义三规则；不可变/可变借用；借用不夺所有权 | 能解释 "cannot move out of borrowed content" |
| 3 | 11-29 | 日 | 切片 + 结构体 + 方法 | Ch4.3 + Ch5 | 切片借用不复制；struct/impl/method；UTF-8 边界 | 写出 `Rectangle` + `area()`，理解字节≠字符 |
| 4 | 11-30 | 一 | **借用检查器实操** | Ch4.2 三类错误 | 借用后修改／借用后移动／借用后销毁；三种修法 | 能读懂并修好 5 个借用错误 |
| 5 | 12-06 | 日 | 枚举与模式匹配 | Ch6 | enum 携带数据；Option；match 穷尽；if let；let-else；`@` 绑定 | 用 Option 建模「可能没有值」，不用空指针 |
| 6 | 12-07 | 一 | 错误处理 | Ch9 | Result；`?` 运算符；From 转换；自定义错误 + Display/Error | 一条 `?` 串联的错误流，错误信息可读 |
| 7 | 12-13 | 日 | 模块与包 | Ch7 | mod/use/pub；可见性层级；嵌套模块；`crate::` 路径 | 把代码拆成模块树并控制可见性 |
| 8 | 12-14 | 一 | 常用集合 | Ch8 | Vec/String/HashMap；entry API 计数惯用法 | 写一个词频统计小工具 |
| 9 | 12-20 | 日 | 泛型与 Trait | Ch10.1-10.2 | 泛型 + trait bound；trait 默认实现；`impl Trait` | 定义 trait 并用泛型函数约束它 |
| 10 | 12-21 | 一 | 生命周期 | Ch10.3 | 省略规则；`<'a>` 标注；结构体持有引用；生命周期约束 | 能判断何时要手写标注、何时能推出来 |
| 11 | 12-27 | 日 | **动手：猜数字游戏** | Ch2 全流程 | 输入容错；比较逻辑抽枚举；随机源做成依赖以便测试 | 一个能玩的游戏 + 5 个通过的测试 |
| 12 | 12-28 | 一 | **动手：CLI 搜索工具** | Ch12 | 读参数/读文件；逻辑抽成可测函数；错误走 stderr；退出码 | 能跑的 minigrep + 3 个通过的测试 |
| 13 | 01-03 | 日 | 机动：补漏 + 阶段复盘 | — | 补欠的块；把 12 个示例串成一篇笔记；列出还不会的点 | 能口头讲清所有权/借用/生命周期三者的关系 |

> 注意块 12 和块 13 之间隔了 4 天（12-28 → 01-03），中间 12-29/30/31 的周一晚块被用作缓冲 —— **这就是"机动块"的意义**：欠账就在这一周补。

### 每块都有可编译的示例

`docs/plans/rust-baseline/` 下 12 个 `.rs`，**全部已用 rustc 编译并跑通测试**：

```
$ sh docs/plans/rust-baseline/run-all.sh
=== block01_basics ===        test result: ok. 3 passed
=== block02_ownership ===     test result: ok. 4 passed
=== block03_structs ===       test result: ok. 3 passed
=== block04_borrow_checker ===test result: ok. 2 passed
=== block05_enums ===         test result: ok. 4 passed
=== block06_errors ===        test result: ok. 4 passed
=== block07_modules ===       test result: ok. 3 passed
=== block08_collections ===   test result: ok. 4 passed
=== block09_traits ===        test result: ok. 4 passed
=== block10_lifetimes ===     test result: ok. 5 passed
=== block11_guessing_game === test result: ok. 5 passed
=== block12_minigrep ===      test result: ok. 3 passed
全部通过
```

合计 **46 个测试**。每个文件顶部的注释写明了这一块对应 Book 的哪几节、以及"本块产出"是什么。

**这些代码是真跑过的**，不是纸面示例：

- `block04` 把三类借用错误以 `compile_fail` 注释原样保留，取消注释即可复现 rustc 报错
- `block11` 把随机源做成参数（依赖注入），所以 5 个测试全部确定性可复现
- `block12` 的 minigrep 有真的退出码语义（找到=0，没找到=1，参数错=2）

### 环境约束（2026-11-19 实测）

- 本机 rustc **1.100.0**（Arch 系统包），**没有 rustup**
- **crates.io 返回 403** → 装不了 rustlings、拉不了任何外部 crate
- 所以示例全部用 **纯 std**，`rustc --edition 2024` 直接编译（Book 明确要求 edition 2024）
- 官方教程 Ch2 用的 `rand` crate 被替换成自带的确定性 LCG —— **原理一致（把随机源做成可注入依赖），实现更朴素**

## 2. 寒假 8 周冲刺（64h）

学期内只能打地基。**寒假才是这门课的主战场** —— 连续 8 周、每天整块。

| 周 | 日期 | 内容 | Rust Book | 产出 |
|---|---|---|---|---|
| W1 | 01-04 ~ 01-10 | 复盘 + 填空式练习（所有权/借用/生命周期） | 补块1-10 | 把欠账补齐，整理成一篇自己的笔记 |
| W2 | 01-11 ~ 01-17 | 迭代器与闭包 | Ch13 | 把 minigrep 改成迭代器版本 |
| W3 | 01-18 ~ 01-24 | 智能指针 Box / Rc / RefCell | Ch15 | 用 `Rc<RefCell<T>>` 写一个共享状态小例子 |
| W4 | 01-25 ~ 01-31 | 并发：线程、消息传递、Send/Sync | Ch16 | 多线程计数器，能讲清数据竞争怎么被编译器挡住 |
| W5 | 02-01 ~ 02-07 | 异步 async / await | Ch17 | 一个异步任务示例 |
| W6 | 02-08 ~ 02-14 | 模式匹配进阶 + 宏 | Ch19 + Ch20.5 | 写一个 `macro_rules!` 小宏 |
| W7 | 02-15 ~ 02-21 | **项目一：多线程 Web 服务器** | Ch21 | 完整跑通 Book 终章项目 |
| W8 | 02-22 ~ 02-28 | **项目二：自选项目** + 复盘 | — | 一个自己写的、能给别人看的东西 |

**总量**：学期内 26h + 寒假 64h = 90h ≈ 完整 Rust 路线的量。

## 3. 如果掉块了，砍哪些

排期假设 13 个块**一个都不落**。真实情况大概率是"13 个来了 8 个"。所以要有取舍顺序：

| 优先级 | 块 | 理由 |
|---|---|---|
| **必修**（缺了等于没学 Rust） | 1, 2, 4, 5, 6, 9 | 语法 → 所有权 → 借用检查器 → 枚举 → 错误 → trait |
| **强烈建议** | 12 | 动手写项目是唯一能检验前面是否真会了的方式 |
| 可选 | 3, 7, 8, 10, 11 | 切片/模块/集合/生命周期/游戏 |
| 寒假可替代 | 10（生命周期）可延到并发周学；7/8 随时可补 |

**砍块 10（生命周期）的代价**：能写业务代码，但读不懂 `&'a str` 这类签名 —— 后面并发（Ch16-17）会因此卡住。所以建议把块 10 挪到寒假 W1，而不是直接砍。

## 4. 验收标准（学期末要能答上来的）

不是"看完了"，是能做出这些：

1. **能读懂并修复借用检查器的三类报错**（块 4）
2. **能用 `Result` + `?` 组合错误流，且错误信息对用户可读**（块 6）
3. **能用 trait 抽象行为、用泛型约束类型**（块 9）
4. **能判断何时需要手写生命周期标注**（块 10）
5. **能从零写出一个带测试的 CLI 工具**（块 12）

对应 Rust Book 官方 Learning Path 的前两档（[Learn Rust](https://github.com/rust-lang/rustlings) → [The Rust Book](https://doc.rust-lang.org/book/)）—— 即"读完 Book 前 10 章 + 做完 minigrep"，这是社区公认的"入门完成"线。

## 5. 怎么复现这次模拟

```bash
# 1. 跑示例（需要 rustc >= 1.85，edition 2024）
sh docs/plans/rust-baseline/run-all.sh

# 2. 跑排期模拟（纯读 sqlite，可在生产机直接跑）
python3 scripts/plan-sim.py --db ~/ru41-mimo-smoke/data/state.db
```

脚本会输出三段：槽位稳定性表 → 固定槽位推荐 → **块级排期（哪天学什么 + 对应示例文件是否存在）**。

## 6. 与长期规划方案的关系

这份规划是 longterm-planning.md 的**第一个真实 B 类目标**。它同时也是那次模拟暴露的 6 个缺陷的活样本：

| 缺陷 | 这份规划里的体现 |
|---|---|
| 抽样看课表会漏冲突 | 槽位由脚本从全量 `weeks` 展开算出，不是看一周课表挑的 |
| 只按容量排会排出连着两天 | 推荐函数加了间隔天数，间隔=0 扣分 |
| 习惯冲突要平移不堆叠 | 块 13 就是机动块；撞课时整体平移而不是堆到某天 |
| 学期边界断档 | 寒假 8 周单独规划，不依赖课表 |
| 目标范围由容量倒推 | 24h 的块级内容是按 26h 容量砍出来的 |
| 上午/晚上要分开 | 槽位表里上午基本全废，只有周末上午干净 |

**下一步不是自动化，而是手工跑 B0**：先按这份规划手工执行两周，看 13 个块到底来了几个。**如果手工都坚持不下来，自动排期只会让一堆排好的块躺在那里没人动。**

## 来源

- Rust Book 目录（章节号以此为准，与旧版不同）：https://raw.githubusercontent.com/rust-lang/book/master/src/SUMMARY.md
- Rust Book 在线版（要求 Rust ≥ 1.98 + edition 2024）：https://doc.rust-lang.org/book/
- Rust 官方 Learning Path：https://github.com/rust-lang/learning
