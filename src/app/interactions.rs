use crate::{domain::records::Interaction, ports::storage::TaskStore, runtime::worker::now};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct InteractionService {
    pub store: Arc<dyn TaskStore>,
}
impl InteractionService {
    /// 记录用户对交互的回复；违反任一答复规则时返回错误且不落库。
    pub fn reply(&self, id: &str, response: Value) -> Result<()> {
        ensure!(
            self.store.reply_interaction(id, &response, now())?,
            "interaction is invalid, already answered, expired or belongs to an inactive attempt"
        );
        Ok(())
    }
    /// 供适配器侧调用：持久化交互请求并轮询等待回复；
    /// 过期时把状态置为 expired 并返回错误，绝不无限阻塞。
    /// 带 dismiss_response 的交互过期自动以该值作答（dismiss 语义：
    /// 用户没答复/关掉了，底座按约定值继续——审批走拒绝，提问走"自行判断"）。
    pub async fn request(&self, mut request: Interaction) -> Result<Value> {
        ensure!(
            self.store.put_document(
                "interaction",
                &request.id,
                None,
                &serde_json::to_value(&request)?
            )?,
            "duplicate interaction"
        );
        loop {
            let doc = self
                .store
                .document("interaction", &request.id)?
                .context("interaction disappeared")?;
            request = serde_json::from_value(doc.value)?;
            if let Some(response) = request.response {
                return Ok(response);
            }
            if request.expires_at <= now() {
                request.status = "expired".into();
                // dismiss：有兜底回复就当作用户主动关闭，运行继续而不是判失败。
                request.response = request.dismiss_response.clone();
                self.store.put_document(
                    "interaction",
                    &request.id,
                    Some(doc.revision),
                    &serde_json::to_value(&request)?,
                )?;
                if let Some(dismiss) = request.dismiss_response {
                    return Ok(dismiss);
                }
                anyhow::bail!("interaction expired");
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
}

impl crate::ports::interaction::InteractionHandler for InteractionService {
    fn request(
        &self,
        request: Interaction,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + '_>> {
        Box::pin(InteractionService::request(self, request))
    }
}

/// 卡片正文里 detail 的最大字符数（QQ 单条消息与阅读体验的双上限）。
pub const CARD_DETAIL_MAX_CHARS: usize = 1_500;
/// 选项按钮上限：超出的裁掉并在正文里说明（参考 OpenMausBot 的 MAX_OPTIONS=12）。
pub const CARD_MAX_OPTIONS: usize = 12;

/// 选项的按钮文案：`label`/`title` 字段优先，否则取第一个标量值，兜底 JSON。
pub fn option_label(option: &Value) -> String {
    if let Some(label) = option["label"].as_str().or(option["title"].as_str()) {
        return crate::util::truncate_chars(label, 20);
    }
    if let Some(object) = option.as_object()
        && let Some((_, value)) = object.iter().next()
    {
        if let Some(text) = value.as_str() {
            return crate::util::truncate_chars(text, 20);
        }
        if let Some(number) = value.as_i64() {
            return number.to_string();
        }
    }
    crate::util::truncate_chars(&option.to_string(), 20)
}

/// 交互卡片：正文（方法 + 截断的 detail + 编号选项）与按钮（`/答复 <id8> <n>`）。
/// 按钮动作统一为代用户发出答复指令，渠道适配器负责翻译成平台键盘。
pub fn render_card(interaction: &Interaction) -> (String, Vec<String>) {
    let short_id: String = interaction.id.chars().take(8).collect();
    let detail = crate::util::truncate_chars(
        &serde_json::to_string_pretty(&interaction.detail).unwrap_or_default(),
        CARD_DETAIL_MAX_CHARS,
    );
    let mut text = format!(
        "## ❓ 任务请求你的答复\n\n- **类型**：`{}`\n- **编号**：`{short_id}`\n",
        interaction.method
    );
    if !detail.trim().is_empty() && detail != "null" {
        text.push_str(&format!("\n```json\n{detail}\n```\n"));
    }
    let mut labels = Vec::new();
    let overflow = interaction.options.len().saturating_sub(CARD_MAX_OPTIONS);
    for (index, option) in interaction
        .options
        .iter()
        .take(CARD_MAX_OPTIONS)
        .enumerate()
    {
        let label = option_label(option);
        text.push_str(&format!("\n{}. {label}", index + 1));
        labels.push(label);
    }
    if overflow > 0 {
        text.push_str(&format!(
            "\n\n> 另有 {overflow} 个选项未展示，用 `/答复 {short_id} <序号>` 查看完整列表"
        ));
    }
    text.push_str(&format!(
        "\n\n> 点按钮或回复 `/答复 {short_id} <序号>`；超时未答复将按预设兜底处理。"
    ));
    (text, labels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn interaction(options: Vec<Value>) -> Interaction {
        Interaction {
            id: "run1-abcdef123456".into(),
            run_id: "run1".into(),
            request_id: json!(7),
            method: "item/commandExecution/requestApproval".into(),
            detail: json!({"command": "rm -rf /tmp/x"}),
            options,
            expires_at: 0,
            status: "pending".into(),
            response: None,
            notify: None,
            dismiss_response: None,
            notified: false,
        }
    }

    #[test]
    fn card_renders_options_with_labels_and_ids() {
        let interaction = interaction(vec![
            json!({"decision": "accept"}),
            json!({"decision": "decline"}),
        ]);
        let (text, labels) = render_card(&interaction);
        assert!(text.contains("run1-ab"));
        assert!(text.contains("1. accept"));
        assert!(text.contains("2. decline"));
        assert!(text.contains("/答复 run1-ab"));
        assert_eq!(labels, vec!["accept", "decline"]);
    }

    #[test]
    fn card_caps_options_and_notes_overflow() {
        let interaction = interaction(
            (0..15)
                .map(|i| json!({"decision": format!("opt{i}")}))
                .collect(),
        );
        let (text, labels) = render_card(&interaction);
        assert_eq!(labels.len(), CARD_MAX_OPTIONS);
        assert!(text.contains("另有 3 个选项未展示"));
    }

    #[test]
    fn option_label_prefers_label_field() {
        assert_eq!(option_label(&json!({"label": "允许", "value": 1})), "允许");
        assert_eq!(option_label(&json!({"decision": "cancel"})), "cancel");
        assert_eq!(option_label(&json!(42)), "42");
    }
}
