use crate::{
    domain::{EnqueueResult, NotifyRoute, Task},
    ports::storage::TaskStore,
};
use anyhow::{Result, ensure};
use std::sync::Arc;

/// 各入口共享同一套校验与持久化边界。
pub struct TaskService {
    store: Arc<dyn TaskStore>,
}

impl TaskService {
    pub fn new(store: Arc<dyn TaskStore>) -> Self {
        Self { store }
    }

    pub fn enqueue(&mut self, request_key: &str, prompt: &str) -> Result<EnqueueResult> {
        self.enqueue_for(request_key, prompt, "codex")
    }

    pub fn enqueue_for(
        &mut self,
        request_key: &str,
        prompt: &str,
        backend: &str,
    ) -> Result<EnqueueResult> {
        validate_input(request_key, prompt, backend)?;
        self.store.enqueue_for(
            request_key,
            prompt,
            backend,
            crate::domain::RunContract::AnalysisV1,
        )
    }

    pub fn enqueue_with_notify(
        &self,
        request_key: &str,
        prompt: &str,
        backend: &str,
        notify: &NotifyRoute,
    ) -> Result<EnqueueResult> {
        validate_input(request_key, prompt, backend)?;
        ensure!(
            !notify.channel.trim().is_empty() && !notify.target.trim().is_empty(),
            "notification route is required"
        );
        self.store.enqueue_with_notify(
            request_key,
            prompt,
            backend,
            crate::domain::RunContract::AnalysisV1,
            notify,
        )
    }

    pub fn get(&self, id: i64) -> Result<Task> {
        ensure!(id > 0, "task id must be positive");
        self.store
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("task not found"))
    }

    pub fn list(&self, limit: u32) -> Result<Vec<Task>> {
        ensure!(
            (1..=1000).contains(&limit),
            "limit must be between 1 and 1000"
        );
        self.store.list(limit)
    }

    pub fn cancel(&mut self, id: i64) -> Result<Task> {
        ensure!(id > 0, "task id must be positive");
        self.store.cancel(id)
    }
    pub fn retry(&mut self, id: i64) -> Result<Task> {
        self.store.retry(id)
    }
    pub fn attempts(&self, id: i64) -> Result<Vec<crate::domain::Attempt>> {
        self.store.attempts(id)
    }
}

fn validate_input(key: &str, prompt: &str, backend: &str) -> Result<()> {
    ensure!(
        !key.trim().is_empty() && key.len() <= 512,
        "request key must contain 1..512 bytes"
    );
    ensure!(
        !prompt.trim().is_empty() && prompt.len() <= 65_536,
        "prompt must contain 1..65536 bytes"
    );
    ensure!(
        !backend.trim().is_empty() && backend.len() <= 128,
        "backend must contain 1..128 bytes"
    );
    Ok(())
}
