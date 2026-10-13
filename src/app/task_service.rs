use crate::{
    adapters::sqlite::TaskStore,
    domain::{EnqueueResult, Task},
};
use anyhow::{Result, ensure};

/// Entry points share the same validation and persistence boundary.
pub struct TaskService {
    store: TaskStore,
}

impl TaskService {
    pub fn new(store: TaskStore) -> Self {
        Self { store }
    }

    pub fn enqueue(&mut self, request_key: &str, prompt: &str) -> Result<EnqueueResult> {
        ensure!(
            !request_key.trim().is_empty(),
            "request key must not be empty"
        );
        ensure!(request_key.len() <= 512, "request key exceeds 512 bytes");
        ensure!(!prompt.trim().is_empty(), "prompt must not be empty");
        ensure!(prompt.len() <= 65_536, "prompt exceeds 65536 bytes");
        self.store.enqueue(request_key, prompt)
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
}
