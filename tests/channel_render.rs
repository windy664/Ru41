//! 跨渠道渲染回归：同一份规范文本，QQ 与 Telegram 各自落成能渲染的形态。
//!
//! 规范层（`channels::builtin` / planner 的 view）只产出中性内容：
//! 标题是通用二级标题、条目是列表项、复选框是可点指令占位符。
//! 渲染层按网关方言适配：QQ 展开行内 cmd-input 并放大字号，其它渠道降级成纯符号。

use ru41::channels::{builtin, markdown, qq::render::qq_render};

/// 已勾选复选框：U+25A3 ▣。
///
/// U+2611 ☑ **是** emoji 字符，Telegram 会渲染成彩色大勾框；
/// 而 U+2610 ☐ 永远是纯文本字形——两个复选框就不成一套了（2026-11-19 修）。
/// 加 VS15 也试过，Telegram 无视；只能用压根不是 emoji 字符的 ▣。
const CHECKED: &str = "\u{25A3}";

/// planner 的待办清单（规范层形态）。
fn canonical_todo_list() -> String {
    format!(
        "## 待办（1）\n\n- {{{{cmd:☐|/完成 abc123}}}} 板子焊接 — 明天\n- {{{{cmd:{CHECKED}|/恢复待办 def456}}}} ~~写周报~~\n"
    )
}

#[test]
fn qq_rendering_keeps_inline_chip_and_font_scaling() {
    let rendered = qq_render(&canonical_todo_list());
    // 栏目标题：一级标题 + 加粗（QQ 没有字号语法，靠标题层级放大）。
    assert!(rendered.starts_with("# **待办（1）**"), "{rendered}");
    // 条目：三级标题 + 行内可点 cmd-input。
    assert!(
        rendered.contains("### <qqbot-cmd-input text=\"%2F%E5%AE%8C%E6%88%90%20abc123\""),
        "{rendered}"
    );
    assert!(rendered.contains("show=\"%E2%98%90\""), "{rendered}");
    assert!(rendered.contains("~~写周报~~"), "{rendered}");
    // 不残留中性占位符。
    assert!(!rendered.contains("{{cmd:"), "{rendered}");
}

#[test]
fn checked_checkbox_is_not_an_emoji_character() {
    let rendered = qq_render(&canonical_todo_list());
    // 已勾选那个：▣（U+25A3 → %E2%96%A3），不是 emoji 字符，跨客户端一致。
    assert!(rendered.contains("show=\"%E2%96%A3\""), "{rendered}");
    // 裸 ☑（%E2%98%91）是 emoji 字符，Telegram 上会变成彩色大勾框——不能出现。
    assert!(!rendered.contains("show=\"%E2%98%91\""), "{rendered}");
}

#[test]
fn telegram_rendering_degrades_chip_to_plain_symbol() {
    let rendered = markdown::render(&canonical_todo_list(), &markdown::TELEGRAM);
    // 标题降级成粗体（Telegram 无多级标题）。
    assert!(rendered.contains("<b>待办（1）</b>"), "{rendered}");
    // 复选框降级成纯文本符号，命令不外泄。
    assert!(rendered.contains("• ☐ 板子焊接 — 明天"), "{rendered}");
    assert!(
        rendered.contains(&format!("{CHECKED} <s>写周报</s>")),
        "{rendered}"
    );
    assert!(!rendered.contains("{{cmd:"), "{rendered}");
    assert!(!rendered.contains("<qqbot-cmd-input"), "{rendered}");
}

#[test]
fn both_channels_share_one_command_vocabulary() {
    // 命令输入不分渠道：同一份词表、同一套解析。
    assert_eq!(
        builtin::parse_command("/", &builtin::DEFAULT),
        Some(builtin::Command::Help),
        "「/」在两个渠道都应是帮助",
    );
    assert_eq!(
        builtin::parse_command("任务", &builtin::DEFAULT),
        Some(builtin::Command::List),
    );
    assert_eq!(
        builtin::parse_command("/help", &builtin::DEFAULT),
        None,
        "不存在 /help 命令：help 的入口是「/」",
    );
}

#[test]
fn table_never_reaches_any_channel() {
    let canonical = "## 标题\n\n| 命令 | 用途 |\n|---|---|\n| `/` | 帮助 |\n";
    for profile in [markdown::QQ, markdown::TELEGRAM] {
        let rendered = markdown::render(canonical, &profile);
        assert!(!rendered.contains("|---"), "{profile:?}: {rendered}");
    }
}
