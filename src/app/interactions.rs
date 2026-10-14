use crate::{domain::records::Interaction, ports::storage::TaskStore, runtime::worker::now};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct InteractionService {
    pub store: Arc<dyn TaskStore>,
}
impl InteractionService {
    pub fn reply(&self, id: &str, response: Value) -> Result<()> {
        let doc = self
            .store
            .document("interaction", id)?
            .context("interaction not found")?;
        let mut request: Interaction = serde_json::from_value(doc.value)?;
        ensure!(
            request.status == "pending" && request.expires_at > now(),
            "interaction is no longer pending"
        );
        let run = self
            .store
            .document("run", &request.run_id)?
            .context("run not found")?;
        ensure!(run.value["status"] == "running", "run is no longer active");
        let task_id = run.value["task_id"].as_i64().context("invalid run task")?;
        let task = self.store.get(task_id)?.context("task not found")?;
        ensure!(
            task.status == crate::domain::TaskStatus::Running
                && task.lease_until.is_some_and(|v| v > now()),
            "task is no longer active"
        );
        ensure!(
            request.options.contains(&response),
            "response is not an offered option"
        );
        request.status = "answered".into();
        request.response = Some(response);
        ensure!(
            self.store.put_document(
                "interaction",
                id,
                Some(doc.revision),
                &serde_json::to_value(request)?
            )?,
            "interaction already answered"
        );
        Ok(())
    }
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
