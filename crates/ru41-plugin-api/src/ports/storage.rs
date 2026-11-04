use crate::domain::{
    AgentResult, Attempt, Claim, EnqueueResult, NotifyRoute, RunContract, Task, TokenUsage,
};
use anyhow::Result;
use std::sync::Arc;

/// 一次尝试的 token 用量记录；用量聚合查询（`usage_since`）返回的行。
/// 四个 token 数为 NULL（底座未上报）的尝试不会出现在查询结果里。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct UsageRecord {
    pub task_id: i64,
    pub attempt_id: i64,
    pub request_key: String,
    pub started_at: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
}

/// 任务与尝试的持久化边界；SQLite 是默认适配器。
/// 实现必须原子地执行 fencing 与租约规则；任务和审核状态变化的事件也必须同事务写入。
pub trait TaskStore: super::consumers::ConsumerStore {
    /// 在同一事务中校验交互、所属运行、当前 attempt 与租约，并记录回复。
    /// 不存在、过期、已回答或失效运行返回 false；存储故障返回错误。
    fn reply_interaction(&self, id: &str, response: &serde_json::Value, now: i64) -> Result<bool>;

    fn document(&self, kind: &str, id: &str) -> Result<Option<crate::domain::records::Document>>;
    fn documents(&self, kind: &str, limit: u32) -> Result<Vec<crate::domain::records::Document>>;
    fn put_document(
        &self,
        kind: &str,
        id: &str,
        expected: Option<i64>,
        value: &serde_json::Value,
    ) -> Result<bool>;

    fn enqueue_for(
        &self,
        request_key: &str,
        prompt: &str,
        backend: &str,
        contract: RunContract,
    ) -> Result<EnqueueResult>;
    fn get(&self, id: i64) -> Result<Option<Task>>;
    fn list(&self, limit: u32) -> Result<Vec<Task>>;
    fn cancel(&self, id: i64) -> Result<Task>;
    /// 手动重试授予新的三次尝试预算，不删除历史。
    fn retry(&self, id: i64) -> Result<Task>;
    fn attempts(&self, id: i64) -> Result<Vec<Attempt>>;
    fn claim_for(
        &self,
        worker: &str,
        backend: &str,
        model: Option<&str>,
        contract: RunContract,
        now: i64,
        lease_secs: i64,
    ) -> Result<Option<Claim>>;
    fn heartbeat(&self, claim: &Claim, now: i64, lease_secs: i64) -> Result<bool>;
    fn attach_run(&self, claim: &Claim, dir: &str, now: i64) -> Result<()>;
    /// 附带终态通知路由的入队，幂等规则与 enqueue_for 相同。
    fn enqueue_with_notify(
        &self,
        request_key: &str,
        prompt: &str,
        backend: &str,
        contract: RunContract,
        notify: &NotifyRoute,
    ) -> Result<EnqueueResult>;
    /// 已到终态、带通知路由且尚未通知的任务，按 id 升序。
    fn list_unnotified(&self, limit: u32) -> Result<Vec<Task>>;
    /// 标记已通知；并发下最多一个调用者得到 true。
    fn mark_notified(&self, id: i64) -> Result<bool>;
    fn succeed(
        &self,
        claim: &Claim,
        result: &AgentResult,
        session: Option<&str>,
        now: i64,
    ) -> Result<bool>;
    /// 终态写入失败记录；`retryable=false` 时跳过重试预算直接 failed。
    /// `interrupted=true`（进程被关停打断）不算真实失败：无条件重排
    /// （实现侧带 sanity 上限），不消耗 max_attempts 的公平预算。
    fn fail(
        &self,
        claim: &Claim,
        error: &str,
        now: i64,
        delay_secs: i64,
        interrupted: bool,
        retryable: bool,
    ) -> Result<bool>;

    /// 记录一次尝试的 token 用量（底座事件流上报）；仅宿主 worker 调用，
    /// 插件桥接不经过此路径。默认空实现，存储适配器负责落库。
    fn record_usage(&self, claim: &Claim, usage: &TokenUsage) -> Result<()> {
        let _ = (claim, usage);
        Ok(())
    }

    /// `started_at >= since` 且底座上报过用量的尝试，按 started_at 升序。
    /// 默认空实现；SQLite/PostgreSQL 适配器负责实现。
    fn usage_since(&self, since: i64) -> Result<Vec<UsageRecord>> {
        let _ = since;
        Ok(Vec::new())
    }

    fn enqueue(&self, request_key: &str, prompt: &str) -> Result<EnqueueResult> {
        self.enqueue_for(request_key, prompt, "codex", RunContract::AnalysisV1)
    }

    fn claim(&self, worker: &str, now: i64, lease_secs: i64) -> Result<Option<Claim>> {
        self.claim_for(
            worker,
            "codex",
            None,
            RunContract::AnalysisV1,
            now,
            lease_secs,
        )
    }
}

impl<T: TaskStore + ?Sized> TaskStore for Arc<T> {
    fn reply_interaction(&self, id: &str, response: &serde_json::Value, now: i64) -> Result<bool> {
        (**self).reply_interaction(id, response, now)
    }

    fn document(&self, kind: &str, id: &str) -> Result<Option<crate::domain::records::Document>> {
        (**self).document(kind, id)
    }
    fn documents(&self, kind: &str, limit: u32) -> Result<Vec<crate::domain::records::Document>> {
        (**self).documents(kind, limit)
    }
    fn put_document(
        &self,
        kind: &str,
        id: &str,
        expected: Option<i64>,
        value: &serde_json::Value,
    ) -> Result<bool> {
        (**self).put_document(kind, id, expected, value)
    }

    fn enqueue_for(
        &self,
        request_key: &str,
        prompt: &str,
        backend: &str,
        contract: RunContract,
    ) -> Result<EnqueueResult> {
        (**self).enqueue_for(request_key, prompt, backend, contract)
    }
    fn get(&self, id: i64) -> Result<Option<Task>> {
        (**self).get(id)
    }
    fn list(&self, limit: u32) -> Result<Vec<Task>> {
        (**self).list(limit)
    }
    fn cancel(&self, id: i64) -> Result<Task> {
        (**self).cancel(id)
    }
    fn retry(&self, id: i64) -> Result<Task> {
        (**self).retry(id)
    }
    fn attempts(&self, id: i64) -> Result<Vec<Attempt>> {
        (**self).attempts(id)
    }
    fn claim_for(
        &self,
        worker: &str,
        backend: &str,
        model: Option<&str>,
        contract: RunContract,
        now: i64,
        lease_secs: i64,
    ) -> Result<Option<Claim>> {
        (**self).claim_for(worker, backend, model, contract, now, lease_secs)
    }
    fn heartbeat(&self, claim: &Claim, now: i64, lease_secs: i64) -> Result<bool> {
        (**self).heartbeat(claim, now, lease_secs)
    }
    fn attach_run(&self, claim: &Claim, dir: &str, now: i64) -> Result<()> {
        (**self).attach_run(claim, dir, now)
    }
    fn enqueue_with_notify(
        &self,
        request_key: &str,
        prompt: &str,
        backend: &str,
        contract: RunContract,
        notify: &NotifyRoute,
    ) -> Result<EnqueueResult> {
        (**self).enqueue_with_notify(request_key, prompt, backend, contract, notify)
    }
    fn list_unnotified(&self, limit: u32) -> Result<Vec<Task>> {
        (**self).list_unnotified(limit)
    }
    fn mark_notified(&self, id: i64) -> Result<bool> {
        (**self).mark_notified(id)
    }
    fn succeed(
        &self,
        claim: &Claim,
        result: &AgentResult,
        session: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        (**self).succeed(claim, result, session, now)
    }
    fn fail(
        &self,
        claim: &Claim,
        error: &str,
        now: i64,
        delay_secs: i64,
        interrupted: bool,
        retryable: bool,
    ) -> Result<bool> {
        (**self).fail(claim, error, now, delay_secs, interrupted, retryable)
    }
    fn record_usage(&self, claim: &Claim, usage: &TokenUsage) -> Result<()> {
        (**self).record_usage(claim, usage)
    }
    fn usage_since(&self, since: i64) -> Result<Vec<UsageRecord>> {
        (**self).usage_since(since)
    }
}
