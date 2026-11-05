//! 手势签到轨迹候选：九键（1-9）上的数字串，形如 `157862`。
//!
//! 平台只在签到窗口内下发正确轨迹（getPPTActiveInfo 的 data.signCode）；
//! 拿不到时（老师每次换图案、或平台不下发）只能枚举候选：
//! 先常见笔画（横竖撇捺折线），再按"一笔写出来的顺滑度"从 `1` 出发枚举。
//!
//! 排序依据是转折代价：直线 0 < 45° 1 < 90° 2 < 135° 3 < 掉头 4，
//! 重复经过同一格额外加罚——真人手写不会来回描。

/// 九键坐标（与手机九宫格一致）：
/// ```text
/// 7 8 9
/// 4 5 6
/// 1 2 3
/// ```
const KEYPAD: [(i32, i32); 9] = [
    (1, 3), // 1
    (2, 3), // 2
    (3, 3), // 3
    (1, 2), // 4
    (2, 2), // 5
    (3, 2), // 6
    (1, 1), // 7
    (2, 1), // 8
    (3, 1), // 9
];

/// 常见笔画/图形（覆盖老师最常用的几种），优先试。
const CURATED: &[&str] = &[
    // 真实用例最优先：2026-11-05 用户手工签到时报的轨迹（老师当时给的图案）。
    "157862", // 横、竖、斜
    "123", "456", "789", "321", "654", "987", "147", "258", "369", "741", "852", "963", "159",
    "357", "951", "753", "1234567", "1472583", "159357", "147369", "258369",
    // 折线（Z / S 形）
    "157", "753", "7531", "1593", "3519", "7891", "1237", "1473", "157963", "178963",
    // 螺旋、圈
    "12369874", "1478963", "1236321", "25814736", "7894561",
    // 真实用例（2026-11-05 用户手工签到时报的轨迹）
    "157862", "178963", "157896", "175869", "185796",
];

/// 单次活动最多试多少条轨迹（窗口只有约 2 分钟，也避免把账号打到限流）。
/// 每条轨迹一次提交（不走滑块重试），12 条约 5-10 秒，留足时间给人工兜底。
pub const MAX_ATTEMPTS: usize = 20;

/// 候选轨迹：去重后的有序列表（人工常见笔画在前，枚举的在后）。
pub fn candidates() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for code in CURATED {
        if seen.insert((*code).to_string()) {
            out.push((*code).to_string());
        }
    }
    for code in enumerate_from_one(600) {
        if seen.insert(code.clone()) {
            out.push(code);
        }
    }
    out
}

/// 从 `1` 出发枚举长度 4..=8 的轨迹，按顺滑度排序，取前 limit 条。
///
/// 规则：不重复相邻格、不立刻掉头（回上一个格）、
/// 转折代价越小越靠前（直线 → 缓转 → 折角 → 掉头）。
pub fn enumerate_from_one(limit: usize) -> Vec<String> {
    let mut scored: Vec<(u32, u32, String)> = Vec::new();
    let mut path: Vec<usize> = vec![0]; // 索引 0 = 数字 1
    walk(&mut path, &mut scored, limit.saturating_mul(8).max(64));
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, code)| code)
        .collect()
}

/// 深度优先枚举并打分。
fn walk(path: &mut Vec<usize>, out: &mut Vec<(u32, u32, String)>, cap: usize) {
    if out.len() >= cap {
        return;
    }
    if path.len() >= 4 {
        let score = smoothness(path);
        out.push((score, path.len() as u32, digits(path)));
    }
    if path.len() >= 8 {
        return;
    }
    let current = *path.last().unwrap();
    let mut nexts: Vec<usize> = (0..9)
        .filter(|n| {
            *n != current
                && !path[..path.len() - 1].contains(n) // 不立刻掉头
                && !path.contains(n) // 一笔不重复经过同一格
        })
        .collect();
    // 先走代价小的方向：枚举结果按代价升序拼接，整体顺序自然平滑优先。
    nexts.sort_by_key(|n| turn_cost(current, *n));
    for next in nexts {
        path.push(next);
        walk(path, out, cap);
        path.pop();
    }
}

/// 平滑度总分（越小越像一笔写出来的）。
fn smoothness(path: &[usize]) -> u32 {
    let mut total = 0u32;
    for window in path.windows(3) {
        total += turn_cost(window[0], window[2]);
    }
    total
}

/// 从 from 走到 to 的转折代价（用中间格估算转角）。
fn turn_cost(from: usize, to: usize) -> u32 {
    let (x1, y1) = KEYPAD[from];
    let (x2, y2) = KEYPAD[to];
    let dx = (x2 - x1) as f32;
    let dy = (y2 - y1) as f32;
    let len = (dx * dx + dy * dy).sqrt();
    if len == 0.0 {
        return 4;
    }
    // 步长越大越像长划（代价低），越短越像点按（代价高）。
    match len as u32 {
        2 => 0, // 直线两格
        1 => 1, // 斜线一格
        _ => 2,
    }
}

/// 路径转成数字串（索引 0..8 → 数字 '1'..'9'）。
fn digits(path: &[usize]) -> String {
    path.iter()
        .map(|i| (b'0' + (i + 1) as u8) as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curated_patterns_come_first() {
        let list = candidates();
        // 已知真实图案排最前，其次是常见笔画（横）。
        assert_eq!(list[0], "157862");
        assert_eq!(list[1], "123");
        assert!(list.contains(&"157862".to_string()));
        assert!(list.len() > 20);
    }

    #[test]
    fn enumeration_starts_at_one_and_never_repeats() {
        let list = enumerate_from_one(200);
        assert!(!list.is_empty());
        for code in &list {
            assert!(code.starts_with('1'), "{code}");
            let digits: Vec<char> = code.chars().collect();
            for pair in digits.windows(2) {
                assert_ne!(pair[0], pair[1], "{code} 有相邻重复");
            }
            let mut sorted = digits.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), digits.len(), "{code} 有重复格");
        }
    }

    #[test]
    fn enumeration_is_ordered_by_smoothness() {
        let list = enumerate_from_one(60);
        // 平滑的（少转折）应该排在前面：整体转折总数单调不增，允许同分。
        let scores: Vec<u32> = list
            .iter()
            .map(|code| {
                let path: Vec<usize> = code
                    .chars()
                    .map(|c| c.to_digit(10).unwrap() as usize - 1)
                    .collect();
                smoothness(&path)
            })
            .collect();
        for pair in scores.windows(2) {
            assert!(pair[0] <= pair[1], "转折代价应非递减: {scores:?}");
        }
    }
}
