use crate::{domain::records::Interaction, ports::agent::RunContext};
use anyhow::Result;
use serde_json::{Value, json};

/// pi 原生交互（select/confirm）→ 宿主交互通道：卡片经 notify 路由发回会话，
/// 用户点按钮（/答复）或超时由 dismiss 兜底——confirm 超时=拒绝、select 超时=
/// 按 pi 协议的 cancelled 语义继续，运行都不中断。
pub(super) async fn pi_ui_interact(context: &RunContext, event: &Value) -> Result<Value> {
    let ui_id = event["id"].clone();
    let method = event["method"].as_str().unwrap_or("");
    let title = event["title"].as_str().unwrap_or("").to_owned();
    let (detail, options, dismiss) = if method == "input" || method == "editor" {
        // 开放题：无选项，用户在 QQ 里的下一条回复即答案（QQ 侧按
        // pending 开放交互路由）；超时按 pi 协议的取消语义继续。
        let mut detail = json!({"title": title});
        if let Some(placeholder) = event["placeholder"].as_str() {
            detail["placeholder"] = json!(placeholder);
        }
        if let Some(prefill) = event["prefill"].as_str() {
            detail["prefill"] = json!(crate::util::truncate_chars(prefill, 500));
        }
        (detail, Vec::new(), json!({"__pi_cancelled": true}))
    } else if method == "select" {
        let options: Vec<Value> = event["options"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|option| json!({"label": option, "value": option}))
                    .collect()
            })
            .unwrap_or_default();
        // 超时 = 用户没选；哨兵值由本适配器翻译成 pi 的 cancelled 响应。
        (
            json!({"title": title}),
            options,
            json!({"__pi_cancelled": true}),
        )
    } else {
        // confirm：超时 = 拒绝（绝不默认放行）。
        (
            json!({"title": title, "message": event["message"]}),
            vec![
                json!({"label": "确认", "confirmed": true}),
                json!({"label": "拒绝", "confirmed": false}),
            ],
            json!({"confirmed": false}),
        )
    };
    let expires_at =
        crate::runtime::worker::now() + event["timeout"].as_i64().unwrap_or(300).clamp(30, 3600);
    let interaction = Interaction {
        id: format!("{}-{}", context.run_id, rand::random::<u64>()),
        run_id: context.run_id.clone(),
        request_id: ui_id.clone(),
        method: format!("pi/{method}"),
        detail,
        options,
        expires_at,
        status: "pending".into(),
        response: None,
        notify: context.notify.clone(),
        dismiss_response: Some(dismiss),
        notified: false,
    };
    let answered = match context.interactions.request(interaction).await {
        Ok(answered) => answered,
        // 交互通道自身异常：按取消回复，pi 走默认路径，不炸运行。
        Err(error) => {
            eprintln!("[pi] ui interaction failed, cancel: {error:#}");
            return Ok(json!({"type":"extension_ui_response","id":ui_id,"cancelled":true}));
        }
    };
    let response = if answered.get("__pi_cancelled").is_some() {
        json!({"type":"extension_ui_response","id":ui_id,"cancelled":true})
    } else if method == "confirm" {
        let confirmed = answered["confirmed"].as_bool().unwrap_or(false);
        json!({"type":"extension_ui_response","id":ui_id,"confirmed":confirmed})
    } else {
        // select / input / editor 统一回 value 文本。
        let value = answered["value"].as_str().unwrap_or_default().to_owned();
        json!({"type":"extension_ui_response","id":ui_id,"value":value})
    };
    Ok(response)
}

#[cfg(test)]
mod tests {
    use crate::domain::records::Interaction;
    use crate::ports::agent::RunContext;
    use crate::ports::interaction::InteractionHandler;
    use anyhow::Result;
    use serde_json::{Value, json};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    /// 脚本化交互通道：把构造的 Interaction 存下来，返回预设答复。
    struct Canned {
        answer: Value,
        seen: std::sync::Mutex<Option<Interaction>>,
    }
    impl InteractionHandler for Canned {
        fn request(
            &self,
            request: Interaction,
        ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>> {
            *self.seen.lock().unwrap() = Some(request);
            let answer = self.answer.clone();
            Box::pin(async move { Ok(answer) })
        }
    }

    fn context(answer: Value) -> (RunContext, Arc<Canned>) {
        let canned = Arc::new(Canned {
            answer,
            seen: std::sync::Mutex::new(None),
        });
        let context = RunContext {
            run_id: "run-test".into(),
            interactions: canned.clone(),
            conversation_id: None,
            resume_session_id: None,
            skills: Vec::new(),
            notify: None,
            store: None,
        };
        (context, canned)
    }

    #[tokio::test]
    async fn select_maps_option_value() {
        let (context, canned) = context(json!({"label": "B", "value": "B"}));
        let event =
            json!({"id": "ui-1", "method": "select", "title": "选哪个", "options": ["A", "B"]});
        let response = super::pi_ui_interact(&context, &event).await.unwrap();
        assert_eq!(response["value"], "B");
        assert!(response.get("cancelled").is_none());
        let seen = canned.seen.lock().unwrap().clone().unwrap();
        assert_eq!(seen.method, "pi/select");
        assert_eq!(seen.options.len(), 2);
        // 超时兜底是取消哨兵，不是某个选项。
        assert!(
            seen.dismiss_response
                .unwrap()
                .get("__pi_cancelled")
                .is_some()
        );
    }

    #[tokio::test]
    async fn select_dismiss_maps_to_cancelled() {
        let (context, _) = context(json!({"__pi_cancelled": true}));
        let event = json!({"id": "ui-2", "method": "select", "title": "选哪个", "options": ["A"]});
        let response = super::pi_ui_interact(&context, &event).await.unwrap();
        assert_eq!(response["cancelled"], true);
    }

    #[tokio::test]
    async fn input_maps_free_text_value_and_cancels_on_dismiss() {
        let (context, canned) = context(json!({"value": "叫 ru41-notes.md 就行"}));
        let event = json!({"id": "ui-4", "method": "input", "title": "文件叫什么？", "placeholder": "name.md"});
        let response = super::pi_ui_interact(&context, &event).await.unwrap();
        assert_eq!(response["value"], "叫 ru41-notes.md 就行");
        let seen = canned.seen.lock().unwrap().clone().unwrap();
        assert_eq!(seen.method, "pi/input");
        assert!(seen.options.is_empty(), "开放题没有选项");
        assert!(
            seen.dismiss_response
                .unwrap()
                .get("__pi_cancelled")
                .is_some()
        );
    }

    #[tokio::test]
    async fn confirm_maps_bool_and_decline_dismiss() {
        let (context, canned) = context(json!({"label": "确认", "confirmed": true}));
        let event =
            json!({"id": "ui-3", "method": "confirm", "title": "确认？", "message": "真的要删吗"});
        let response = super::pi_ui_interact(&context, &event).await.unwrap();
        assert_eq!(response["confirmed"], true);
        let seen = canned.seen.lock().unwrap().clone().unwrap();
        // confirm 超时 = 拒绝，绝不默认放行。
        assert_eq!(seen.dismiss_response.unwrap()["confirmed"], false);
    }
}
