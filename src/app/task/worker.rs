use crate::{
    app::{
        agent::{AgentBackend, AgentOutput, AgentRequest, BackendRegistry},
        sanitize::Sanitizer,
        store::TaskStore,
    },
    domain::{AgentResult, Claim, RunContract, TaskStatus},
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

/// Worker 持有底座注册表，按任务绑定的 backend_id 路由执行。
pub struct Worker {
    pub store: Arc<dyn TaskStore>,
    pub backends: Arc<BackendRegistry>,
    pub sanitizer: Arc<dyn Sanitizer>,
    pub config: WorkerConfig,
}

impl Worker {
    pub async fn run_once(
        &mut self,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<Option<WorkReport>> {
        self.config.validate()?;
        if *shutdown.borrow() {
            return Ok(None);
        }
        // 依次按各底座的契约认领任务；任务执行时按其 backend_id 取底座。
        let mut claim = None;
        for descriptor in self.backends.descriptors() {
            if !descriptor.contracts.contains(&RunContract::AnalysisV1) {
                continue;
            }
            claim = self.store.claim_for(
                &self.config.worker_id,
                &descriptor.id,
                descriptor.model.as_deref(),
                RunContract::AnalysisV1,
                now(),
                self.config.lease_secs,
            )?;
            if claim.is_some() {
                break;
            }
        }
        let Some(claim) = claim else {
            return Ok(None);
        };
        let agent = match self.backends.get(&claim.task.backend_id) {
            Ok(agent) => agent,
            // 认领按注册底座过滤，正常不会走到这里；防御性记为失败。
            Err(error) => {
                self.store
                    .fail(&claim, &format!("{error:#}"), now(), 86400, false)?;
                let status = self
                    .store
                    .get(claim.task.id)?
                    .context("task disappeared")?
                    .status;
                return Ok(Some(WorkReport {
                    task_id: claim.task.id,
                    attempt_id: claim.attempt_id,
                    status,
                }));
            }
        };
        let result = self.run_claim(&claim, agent, &mut shutdown).await;
        if let Err(error) = result {
            let interrupted = *shutdown.borrow();
            let multiplier = 1_i64 << (claim.task.attempt_count - 1).clamp(0, 10);
            let delay = (self.config.retry_delay_secs * multiplier).min(86400);
            // fenced 写入：已取消或转交的任务不会在这里被覆盖。
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
        agent: Arc<dyn AgentBackend>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        std::fs::create_dir_all(&self.config.runs_dir)?;
        let root = std::fs::canonicalize(&self.config.runs_dir)?;
        // 唯一且私有的尝试目录也避免跨数据库的 ID 冲突。
        let run_dir = tempfile::Builder::new()
            .prefix(&format!(
                "task-{}-attempt-{}-",
                claim.task.id, claim.attempt_id
            ))
            .tempdir_in(root)?
            .keep();
        self.store
            .attach_run(claim, &run_dir.to_string_lossy(), now())?;
        // 脱敏在 Agent 边界完成：映射表只写入本地尝试目录（即审计记录）。
        let (prompt, mapping) = self.sanitizer.sanitize(&claim.task.prompt);
        if !mapping.is_empty() {
            std::fs::write(
                run_dir.join("mapping.json"),
                serde_json::to_string_pretty(&mapping)?,
            )?;
            eprintln!(
                "[sanitize] task {} masked rules: {:?}",
                claim.task.id,
                mapping.rule_counts()
            );
        }
        let request = AgentRequest {
            prompt,
            run_dir,
            contract: claim.task.contract,
        };
        let execution = agent.run(request);
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
                    // 回填真值后再校验落库，库里与渠道通知都是真实内容。
                    let output = AgentOutput {
                        result: AgentResult {
                            summary: self.sanitizer.restore(&output.result.summary, &mapping),
                        },
                        session_id: output.session_id,
                    };
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
