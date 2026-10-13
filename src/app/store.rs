use crate::domain::{AgentResult, Attempt, Claim, EnqueueResult, RunContract, Task};
use anyhow::Result;
use std::sync::Arc;

/// Persistence boundary for tasks and attempts; SQLite is the default adapter.
/// Implementations must apply the fencing and leasing rules atomically.
pub trait TaskStore: Send + Sync {
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
    /// Manual retry grants a new three-attempt budget without removing history.
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
    fn succeed(
        &self,
        claim: &Claim,
        result: &AgentResult,
        session: Option<&str>,
        now: i64,
    ) -> Result<bool>;
    fn fail(
        &self,
        claim: &Claim,
        error: &str,
        now: i64,
        delay_secs: i64,
        interrupted: bool,
    ) -> Result<bool>;

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
    ) -> Result<bool> {
        (**self).fail(claim, error, now, delay_secs, interrupted)
    }
}
