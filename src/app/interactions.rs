use crate::{domain::records::Interaction, ports::storage::TaskStore, runtime::worker::now};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct InteractionService {
    pub store: Arc<dyn TaskStore>,
}
impl InteractionService {
    /// 记录用户对交互的回复；违反任一裁决规则时返回错误且不落库。
    pub fn reply(&self, id: &str, response: Value) -> Result<()> {
        ensure!(
            self.store.reply_interaction(id, &response, now())?,
            "interaction is invalid, already answered, expired or belongs to an inactive attempt"
        );
        Ok(())
    }
    /// 供适配器侧调用：持久化交互请求并轮询等待回复；
    /// 过期时把状态置为 expired 并返回错误，绝不无限阻塞。
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
                self.store.put_document(
                    "interaction",
                    &request.id,
                    Some(doc.revision),
                    &serde_json::to_value(&request)?,
                )?;
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
