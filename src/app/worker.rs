use crate::{
    app::{
        agent_service::{AgentBackend, AgentRequest},
        store::TaskStore,
    },
    domain::{Claim, RunContract, TaskStatus},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

pub struct WorkerConfig {
    pub worker_id: String,
    pub runs_dir: PathBuf,
    pub timeout: Duration,
    pub lease_secs: i64,
    pub retry_delay_secs: i64,
}

impl WorkerConfig {
    fn validate(&self) -> Result<()> {
        ensure!(!self.worker_id.is_empty(), "worker ID is required");
        ensure!(
            (10..=86400).contains(&self.lease_secs),
            "lease must be 10..86400 seconds"
        );
        ensure!(!self.timeout.is_zero(), "timeout must be positive");
        ensure!(
            (0..=86400).contains(&self.retry_delay_secs),
            "retry delay must be 0..86400 seconds"
        );
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct WorkReport {
    pub task_id: i64,
    pub attempt_id: i64,
    pub status: TaskStatus,
}

pub struct Worker<A> {
    pub store: Arc<dyn TaskStore>,
    pub agent: A,
    pub config: WorkerConfig,
}

impl<A: AgentBackend> Worker<A> {
    pub async fn run_once(
        &mut self,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<Option<WorkReport>> {
        self.config.validate()?;
        if *shutdown.borrow() {
            return Ok(None);
        }
        let backend = self.agent.descriptor();
        ensure!(
            backend.contracts.contains(&RunContract::AnalysisV1),
            "backend does not support analysis_v1"
        );
        let Some(claim) = self.store.claim_for(
            &self.config.worker_id,
            &backend.id,
            backend.model.as_deref(),
            RunContract::AnalysisV1,
            now(),
            self.config.lease_secs,
        )?
        else {
            return Ok(None);
        };
        let result = self.run_claim(&claim, &mut shutdown).await;
        if let Err(error) = result {
            let interrupted = *shutdown.borrow();
            let multiplier = 1_i64 << (claim.task.attempt_count - 1).clamp(0, 10);
            let delay = (self.config.retry_delay_secs * multiplier).min(86400);
            // Fenced: a cancelled or reassigned task is never overwritten here.
            self.store
                .fail(&claim, &format!("{error:#}"), now(), delay, interrupted)?;
        }
        let status = self
            .store
            .get(claim.task.id)?
            .context("task disappeared")?
            .status;
        Ok(Some(WorkReport {
            task_id: claim.task.id,
            attempt_id: claim.attempt_id,
            status,
        }))
    }

    async fn run_claim(
        &mut self,
        claim: &Claim,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        std::fs::create_dir_all(&self.config.runs_dir)?;
        let root = std::fs::canonicalize(&self.config.runs_dir)?;
        // Unique, private attempt directories also avoid cross-database ID collisions.
        let run_dir = tempfile::Builder::new()
            .prefix(&format!(
                "task-{}-attempt-{}-",
                claim.task.id, claim.attempt_id
            ))
            .tempdir_in(root)?
            .keep();
        self.store
            .attach_run(claim, &run_dir.to_string_lossy(), now())?;
        let request = AgentRequest {
            prompt: claim.task.prompt.clone(),
            run_dir,
            contract: claim.task.contract,
        };
        let execution = self.agent.run(request);
        tokio::pin!(execution);
        let timeout = tokio::time::sleep(self.config.timeout);
        tokio::pin!(timeout);
        let mut poll = tokio::time::interval(Duration::from_secs(1));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => anyhow::bail!("worker shutting down"),
                _ = &mut timeout => anyhow::bail!("Agent execution timed out"),
                _ = poll.tick() => {
                    ensure!(!*shutdown.borrow(), "worker shutting down");
                    ensure!(self.store.heartbeat(claim, now(), self.config.lease_secs)?, "task cancelled or lease lost");
                }
                result = &mut execution => {
                    let output = result?;
                    output.result.validate().map_err(anyhow::Error::msg)?;
                    ensure!(self.store.succeed(claim, &output.result, output.session_id.as_deref(), now())?, "task cancelled or lease lost before commit");
                    return Ok(());
                }
            }
        }
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
