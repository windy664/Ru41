//! 统一外发消息卡片：所有面向用户的 markdown 消息从这里构建，保证视觉一致。
//!
//! QQ 机器人 markdown 子集约束：支持 `##` 标题、`-` 列表、`>` 引用、`**粗体**`
//! 与链接，不支持表格。本模块的输出保证落在子集内：
//!
//! ```text
//! ## {icon} {title}
//!
//! - {icon} {条目}
//! - {icon} {条目}
//!
//! > {底部指路}
//! ```

/// 一张外发消息卡片。
pub struct Card {
    title: String,
    lines: Vec<String>,
    hint: Option<String>,
}

impl Card {
    /// 标题建议带 emoji 图标，如 `Card::new("📚 作业提醒（3）")`。
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            lines: Vec::new(),
            hint: None,
        }
    }

    /// 一条状态条目：`- {icon} {text}`，图标后自动补空格（不再出现「✅国家安全教育」）。
    pub fn item(&mut self, icon: &str, text: impl AsRef<str>) -> &mut Self {
        self.lines.push(format!("- {} {}", icon, text.as_ref()));
        self
    }

    /// 原样输出一行（小节标题、分隔等已排版内容）。
    pub fn raw(&mut self, line: impl Into<String>) -> &mut Self {
        self.lines.push(line.into());
        self
    }

    /// 底部操作指路，渲染为 `> {hint}`。内容应是「还能做什么」，
    /// 而非命令教学——可按钮化的操作必须挂按钮，不走文字指路。
    pub fn hint(&mut self, hint: impl Into<String>) -> &mut Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn build(&self) -> String {
        let mut out = format!("## {}\n", self.title);
        if !self.lines.is_empty() {
            out.push('\n');
            out.push_str(&self.lines.join("\n"));
        }
        if let Some(hint) = &self.hint {
            out.push_str(&format!("\n\n> {hint}"));
        }
        out
    }
}
