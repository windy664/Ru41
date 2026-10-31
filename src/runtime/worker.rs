use crate::{
    domain::{
        error::{FailureKind, Ru41Error, classify},
        {AgentResult, Claim, RunContract, TaskStatus},
    },
    ports::agent::{AgentBackend, AgentOutput, AgentRequest, BackendRegistry},
    ports::storage::TaskStore,
    privacy::Sanitizer,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::watch;

/// Worker 执行配置：标识、运行目录、超时与租约/重试参数。
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
/// 一次任务执行的报告（任务、尝试与终态）。
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
    /// 配置了 [knowledge] 时挂载：每条用户消息执行前自动检索知识库并注入上下文。
    pub knowledge: Option<Arc<crate::app::knowledge::KnowledgeService>>,
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
                    .fail(&claim, &format!("{error:#}"), now(), 86400, false, false)?;
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
        let run_id = format!("run-{}-{}", claim.task.id, claim.attempt_id);
        let mut run = crate::domain::records::AgentRun {
            id: run_id.clone(),
            task_id: claim.task.id,
            attempt_id: claim.attempt_id,
            backend_id: claim.task.backend_id.clone(),
            status: "running".into(),
            session_id: None,
        };
        self.store
            .put_document("run", &run_id, None, &serde_json::to_value(&run)?)?;
        // 进度通知不再写 started（用户直接看活动流原生消息）；失败路径仍会写 retry。
        let result = self.run_claim(&claim, agent, &mut shutdown).await;
        if let Ok(session_id) = &result {
            run.session_id = session_id.clone();
        }
        run.status = if result.is_ok() {
            "succeeded"
        } else {
            "failed"
        }
        .into();
        if let Some(doc) = self.store.document("run", &run_id)? {
            self.store.put_document(
                "run",
                &run_id,
                Some(doc.revision),
                &serde_json::to_value(&run)?,
            )?;
        }

        if let Err(error) = result {
            let interrupted = *shutdown.borrow();
            // 永久失败不消耗重试预算；未知错误默认瞬态。
            let kind = classify(&error);
            let multiplier = 1_i64 << (claim.task.attempt_count - 1).clamp(0, 10);
            // 部署/重启打断的任务尽快重排，不走指数退避（它没消耗公平预算）。
            let delay = if interrupted {
                30
            } else {
                (self.config.retry_delay_secs * multiplier).min(86400)
            };
            eprintln!(
                "[worker] task {} attempt {} failed ({kind:?}): {error:#}",
                claim.task.id, claim.attempt_id
            );
            // fenced 写入：已取消或转交的任务不会在这里被覆盖。
            self.store.fail(
                &claim,
                &format!("{error:#}"),
                now(),
                delay,
                interrupted,
                kind == FailureKind::Transient,
            )?;
            // 首次失败进入重试时留一条进度通知，避免用户在重试窗口里完全看不到动静。
            // 被部署/重启打断（interrupted）是基础设施噪音，不发通知，任务会静默重排。
            let retrying = self
                .store
                .get(claim.task.id)?
                .map(|task| task.status == TaskStatus::RetryScheduled)
                .unwrap_or(false);
            if retrying && !interrupted && claim.task.attempt_count == 1 {
                let mut reason = format!("{error:#}");
                reason.truncate(300);
                if let Err(doc_error) = self.store.put_document(
                    "progress",
                    &format!("{run_id}-retry"),
                    None,
                    &serde_json::json!({
                        "task_id": claim.task.id,
                        "attempt_id": claim.attempt_id,
                        "kind": "retry",
                        "status": "pending",
                        "error": reason,
                    }),
                ) {
                    eprintln!(
                        "[worker] retry progress doc for task {} failed: {doc_error:#}",
                        claim.task.id
                    );
                }
            }
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
    ) -> Result<Option<String>> {
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
        // 映射仅留宿主内存，不作为 Agent 工作目录中的文件暴露。
        let (prompt, mapping) = self.sanitizer.sanitize(&claim.task.prompt);
        if !mapping.is_empty() {
            eprintln!(
                "[sanitize] task {} masked rules: {:?}",
                claim.task.id,
                mapping.rule_counts()
            );
        }
        // RAG 自动注入：每条消息先查知识库，命中达阈值就把资料前置进 prompt，
        // 模型不必自己想到去调 vault-search。检索故障降级为无注入，不影响任务。
        let prompt = match &self.knowledge {
            Some(knowledge) => match knowledge.retrieval_context(&prompt) {
                Ok(Some(context)) => {
                    eprintln!(
                        "[knowledge] task {} injected retrieval context",
                        claim.task.id
                    );
                    format!("{context}\n---\n用户消息：\n{prompt}")
                }
                Ok(None) => prompt,
                Err(error) => {
                    eprintln!(
                        "[knowledge] task {} retrieval failed: {error:#}",
                        claim.task.id
                    );
                    prompt
                }
            },
            None => prompt,
        };
        let conversation_id = claim
            .task
            .notify
            .as_ref()
            .map(|route| format!("{}:{}", route.channel, route.target));
        let conversation = match &conversation_id {
            Some(id) => self.store.document("conversation", id)?,
            None => None,
        };
        // 会话续接按 底座+模型 双重匹配：换供应商/换模型后旧会话对供应商侧
        // 不再有效（resume 会直接报错），不匹配就从头开始新会话。
        // 旧文档没有 model 字段时视为不匹配（None != Some(model)）。
        let current_model = agent.descriptor().model.clone();
        let resume_session_id = conversation.as_ref().and_then(|doc| {
            let backend_match =
                doc.value["backend_id"].as_str() == Some(claim.task.backend_id.as_str());
            let doc_model = doc
                .value
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            if !backend_match || doc_model != current_model {
                return None;
            }
            doc.value["session_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        });
        let resumed = resume_session_id.is_some();
        let run_id = format!("run-{}-{}", claim.task.id, claim.attempt_id);
        let request = AgentRequest {
            context: Some(crate::ports::agent::RunContext {
                run_id: run_id.clone(),
                interactions: Arc::new(crate::app::interactions::InteractionService {
                    store: self.store.clone(),
                }),
                conversation_id: conversation_id.clone(),
                resume_session_id,
                skills: self.backends.skills.clone(),
            }),
            prompt,
            run_dir: run_dir.clone(),
            contract: claim.task.contract,
        };
        // 活动流推送器：尾随底座的 events.jsonl，把执行过程实时写进 store，
        // QQ 网关的通知循环扫描后发回会话（像正常使用 codex 那样可见）。
        let (activity_stop, activity_stop_rx) = watch::channel(false);
        let streamer = crate::runtime::activity::ActivityStreamer {
            store: self.store.clone(),
            run_id: run_id.clone(),
            task_id: claim.task.id,
            attempt_id: claim.attempt_id,
            path: run_dir.join("events.jsonl"),
            interval: Duration::from_secs(4),
            max_chunks: 40,
        };
        let stream_handle = tokio::spawn(async move { streamer.run(activity_stop_rx).await });
        let execution = agent.run(request);
        tokio::pin!(execution);
        let timeout = tokio::time::sleep(self.config.timeout);
        tokio::pin!(timeout);
        let mut poll = tokio::time::interval(Duration::from_secs(1));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // loop/break 结构：所有退出路径统一在循环外停掉活动流，避免泄漏推送任务。
        let outcome: Result<Option<String>> = loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => break Err(anyhow::anyhow!("worker shutting down")),
                _ = &mut timeout => break Err(Ru41Error::Timeout.into()),
                _ = poll.tick() => {
                    if *shutdown.borrow() {
                        break Err(anyhow::anyhow!("worker shutting down"));
                    }
                    match self.store.heartbeat(claim, now(), self.config.lease_secs) {
                        Ok(true) => {}
                        Ok(false) => break Err(anyhow::anyhow!("task cancelled or lease lost")),
                        Err(error) => break Err(error),
                    }
                }
                result = &mut execution => {
                    match result {
                        Ok(output) => {
                            // 回填真值后再校验落库，库里与渠道通知都是真实内容。
                            let output = AgentOutput {
                                result: AgentResult {
                                    summary: self.sanitizer.restore(&output.result.summary, &mapping),
                                },
                                session_id: output.session_id,
                            };
                            if let Err(error) = output.result.validate() {
                                break Err(Ru41Error::InvalidOutput(error).into());
                            }
                            match self.store.succeed(claim, &output.result, output.session_id.as_deref(), now()) {
                                Ok(true) => break Ok(output.session_id),
                                Ok(false) => break Err(anyhow::anyhow!("task cancelled or lease lost before commit")),
                                Err(error) => break Err(error),
                            }
                        }
                        Err(error) => break Err(error),
                    }
                }
            }
        };
        // 停活动流并等它 flush 完剩余行（丢弃 handle 会让最后的 chunk 丢失）。
        let _ = activity_stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(6), stream_handle).await;
        // 自愈：本次是续接旧会话且执行失败，说明供应商侧会话可能已失效
        // （换 provider、会话过期等），清掉会话让下一条消息从头开始。
        if outcome.is_err()
            && resumed
            && let Some(id) = &conversation_id
            && let Ok(Some(doc)) = self.store.document("conversation", id)
        {
            let mut value = doc.value.clone();
            value["session_id"] = serde_json::Value::Null;
            if let Err(error) =
                self.store
                    .put_document("conversation", id, Some(doc.revision), &value)
            {
                eprintln!("[worker] conversation self-heal failed: {error:#}");
            }
        }
        let session_id = outcome?;
        if let (Some(id), Some(session)) = (&conversation_id, &session_id) {
            let record = serde_json::json!({
                "backend_id": claim.task.backend_id,
                "session_id": session,
                "model": current_model,
            });
            let revision = conversation.as_ref().map(|doc| doc.revision);
            match self
                .store
                .put_document("conversation", id, revision, &record)
            {
                Ok(true) => {}
                Ok(false) => eprintln!("[worker] conversation state update conflict"),
                Err(error) => eprintln!("[worker] conversation state update failed: {error:#}"),
            }
        }
        Ok(session_id)
    }
}

/// 时间源已上移到 util；此处保留 re-export 兼容既有调用方。
pub use crate::util::now;
