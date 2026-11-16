//! Markdown 输出规范：给所有底座统一的格式要求。
//!
//! 规范只约束自然语言回复；结构化契约（如 analysis_v1 的 JSON summary）
//! 仍按契约本身输出，不受这里的排版要求影响。
//!
//! 注入点选在 Worker 组装提示词处（底座无关）：Codex 与 Pi 都拿到同一份规范，
//! 避免各适配器各写一份、越改越不一样。

/// 输出规范正文（注入到提示词尾部）。
///
/// 这是**唯一的、渠道无关**的输出规范：内容落在所有 bot 平台的 Markdown 交集里
/// （标题、列表、粗体、代码、引用、链接），由 `channels::markdown` 渲染层按
/// 各网关方言适配（表格/公式在渲染层统一降级，不指望模型按渠道调整）。
pub const OUTPUT_STYLE: &str = "\
输出规范（自然语言回复时遵守）：
- 数学公式一律用 LaTeX：行内公式写 $...$，独立成行写 $$...$$；
  不要用纯文本符号拼公式（x² 写 $x^2$，√2 写 $\\sqrt{2}$，≥ 写 $\\ge$，≠ 写 $\\ne$）。
- 标题用 # ## ###，无序列表用 -，有序列表用 1.，代码块标注语言。
- 段落之间空一行；不使用 HTML 标签。
- **不要用表格**：bot 平台（QQ / Telegram / Slack / Discord 等）都不渲染 Markdown
  表格，写表只会被降级成条目。要对比多项信息就用列表，字段之间用 `·` 连接。";

/// 给提示词追加输出规范；已包含时原样返回，不重复追加。
pub fn with_output_style(prompt: &str) -> String {
    if prompt.contains(OUTPUT_STYLE) {
        return prompt.to_owned();
    }
    format!("{prompt}\n\n{OUTPUT_STYLE}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_style_once() {
        let once = with_output_style("请回答");
        assert!(once.contains("LaTeX"));
        assert!(once.starts_with("请回答"));
        let twice = with_output_style(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn style_requires_latex_for_formulas() {
        assert!(OUTPUT_STYLE.contains("$...$"));
        assert!(OUTPUT_STYLE.contains("$$...$$"));
        assert!(OUTPUT_STYLE.contains("\\sqrt{2}"));
    }
}
