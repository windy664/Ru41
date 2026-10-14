use crate::{
    domain::{EnqueueResult, NotifyRoute, Task},
    ports::storage::TaskStore,
};
use anyhow::{Result, ensure};
use std::sync::Arc;

/// 各入口共享同一套校验与持久化边界。
/// 任务用例：所有入口（CLI/QQ/HTTP）共享的入队、查询与状态操作边界。
/// 输入校验集中在这里，渠道不得绕过本服务直写存储。
pub struct TaskService {
    store: Arc<dyn TaskStore>,
}

impl TaskService {
    /// 以给定的存储实现构造服务。
    pub fn new(store: Arc<dyn TaskStore>) -> Self {
        Self { store }
    }

    /// 以默认底座（codex）入队；同 key 重复提交幂等返回既有任务。
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

    /// 查询单个任务；不存在时报错。
    pub fn get(&self, id: i64) -> Result<Task> {
        ensure!(id > 0, "task id must be positive");
        self.store
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("task not found"))
    }

    /// 按 id 降序列出最近任务。
    pub fn list(&self, limit: u32) -> Result<Vec<Task>> {
        ensure!(
            (1..=1000).contains(&limit),
            "limit must be between 1 and 1000"
        );
        self.store.list(limit)
    }

    /// 取消排队或运行中的任务；重复取消安全。
    pub fn cancel(&mut self, id: i64) -> Result<Task> {
        ensure!(id > 0, "task id must be positive");
        self.store.cancel(id)
    }
    /// 手动重试失败任务，追加三次尝试预算。
    pub fn retry(&mut self, id: i64) -> Result<Task> {
        self.store.retry(id)
    }
    /// 列出任务的全部执行尝试（含错误与运行目录）。
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
