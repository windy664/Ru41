//! CAS 验证码求解器的 fixtures 回归（求解器本体在核心 adapters::cas，
//! fixtures 随插件自包含）。校方换验证码风格时更新 tests/fixtures/jwxt_captcha/。

use ru41::adapters::cas::solve;

/// 与核心 cas.rs 内同款的小表达式求值（fixture 标注形如 "7+8"）。
fn evaluate(chars: &str) -> Option<String> {
    let chars = chars.strip_suffix('=')?;
    let (a, rest) = chars.split_at(1);
    let (op, b) = rest.split_at(1);
    let a: u32 = a.parse().ok()?;
    let b: u32 = b.parse().ok()?;
    let result = match op {
        "+" => a + b,
        "*" | "×" => a * b,
        "-" => a.checked_sub(b)?,
        _ => return None,
    };
    Some(result.to_string())
}

#[test]
fn solves_all_labeled_fixtures() {
    let labels: std::collections::BTreeMap<String, String> = serde_json::from_str(
        &std::fs::read_to_string("tests/fixtures/jwxt_captcha/labels.json").unwrap(),
    )
    .unwrap();
    assert_eq!(labels.len(), 40);
    for (name, expr) in &labels {
        let png = std::fs::read(format!("tests/fixtures/jwxt_captcha/{name}.png")).unwrap();
        let expected = evaluate(&format!("{expr}=")).unwrap();
        let got = solve(&png).unwrap_or_else(|| panic!("fixture {name} ({expr}) 未识别"));
        assert_eq!(got, expected, "fixture {name} ({expr}) 解错");
    }
}

#[test]
fn garbage_returns_none_not_panic() {
    assert_eq!(solve(b"not a png"), None);
    assert_eq!(solve(&[]), None);
}
