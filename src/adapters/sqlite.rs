use crate::domain::{AgentResult, Attempt, Claim, EnqueueResult, RunContract, Task};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use std::{path::Path, time::Duration};

pub struct TaskStore {
    connection: Connection,
}

impl TaskStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).context("cannot create database directory")?;
        }
        let mut connection = Connection::open(path).context("cannot open task database")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        // Serialize startup migration with other processes before reading the version.
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        match version {
            0 => {
                tx.execute_batch(include_str!("../../migrations/001_tasks.sql"))?;
                tx.execute_batch(include_str!("../../migrations/002_execution.sql"))?;
            }
            1 => tx.execute_batch(include_str!("../../migrations/002_execution.sql"))?,
            2 | 3 => {}
            other => bail!("unsupported database version {other}; expected 3"),
        }
        if version < 3 {
            tx.execute_batch(include_str!("../../migrations/003_backends.sql"))?;
        }
        tx.commit()?;
        Ok(Self { connection })
    }

    pub fn enqueue(&mut self, request_key: &str, prompt: &str) -> Result<EnqueueResult> {
        self.enqueue_for(request_key, prompt, "codex", RunContract::AnalysisV1)
    }

    pub fn enqueue_for(
        &mut self,
        request_key: &str,
        prompt: &str,
        backend: &str,
        contract: RunContract,
    ) -> Result<EnqueueResult> {
        anyhow::ensure!(
            !backend.trim().is_empty() && backend.len() <= 128,
            "invalid backend ID"
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "INSERT INTO tasks (request_key, prompt, backend_id, contract) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(request_key) DO NOTHING",
            params![request_key, prompt, backend, contract.as_str()],
        )?;
        let task = tx.query_row(
            "SELECT * FROM tasks WHERE request_key = ?1",
            [request_key],
            task_from_row,
        )?;
        if task.prompt != prompt || task.backend_id != backend || task.contract != contract {
            bail!(
                "request key already belongs to a different prompt or backend contract; use a new key"
            );
        }
        tx.commit()?;
        Ok(EnqueueResult {
            created: changed == 1,
            task,
        })
    }

    pub fn get(&self, id: i64) -> Result<Option<Task>> {
        Ok(self
            .connection
            .query_row("SELECT * FROM tasks WHERE id = ?1", [id], task_from_row)
            .optional()?)
    }

    pub fn list(&self, limit: u32) -> Result<Vec<Task>> {
        let mut statement = self
            .connection
            .prepare("SELECT * FROM tasks ORDER BY id DESC LIMIT ?1")?;
        let tasks = statement
            .query_map([limit], task_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tasks)
    }

    pub fn cancel(&mut self, id: i64) -> Result<Task> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = get_task(&tx, id)?.context("task not found")?;
        if matches!(
            task.status,
            crate::domain::TaskStatus::Succeeded | crate::domain::TaskStatus::Failed
        ) {
            bail!("terminal task cannot be cancelled");
        }
        tx.execute("UPDATE attempts SET status='cancelled', finished_at=unixepoch(), error='cancelled by user' WHERE task_id=?1 AND status='running'", [id])?;
        tx.execute("UPDATE tasks SET status='cancelled', active_attempt_id=NULL, lease_until=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1", [id])?;
        let task = get_task(&tx, id)?.context("task not found")?;
        tx.commit()?;
        Ok(task)
    }

    /// Manual retry grants a new three-attempt budget without removing history.
    pub fn retry(&mut self, id: i64) -> Result<Task> {
        let changed = self.connection.execute("UPDATE tasks SET status='queued', available_at=0, max_attempts=attempt_count+3, last_error=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1 AND status='failed'", [id])?;
        anyhow::ensure!(changed == 1, "only a failed task can be retried");
        self.get(id)?.context("task not found")
    }

    pub fn attempts(&self, id: i64) -> Result<Vec<Attempt>> {
        self.get(id)?.context("task not found")?;
        let mut q = self
            .connection
            .prepare("SELECT * FROM attempts WHERE task_id=?1 ORDER BY id")?;
        Ok(q.query_map([id], |r| {
            Ok(Attempt {
                id: r.get("id")?,
                task_id: r.get("task_id")?,
                worker_id: r.get("worker_id")?,
                status: r.get("status")?,
                started_at: r.get("started_at")?,
                finished_at: r.get("finished_at")?,
                run_dir: r.get("run_dir")?,
                session_id: r.get("session_id")?,
                backend_id: r.get("backend_id")?,
                model: r.get("model")?,
                contract: contract_column(r)?,
                result: json_column(r, "result_json")?,
                error: r.get("error")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn claim(&mut self, worker: &str, now: i64, lease_secs: i64) -> Result<Option<Claim>> {
        self.claim_for(
            worker,
            "codex",
            None,
            RunContract::AnalysisV1,
            now,
            lease_secs,
        )
    }

    pub fn claim_for(
        &mut self,
        worker: &str,
        backend: &str,
        model: Option<&str>,
        contract: RunContract,
        now: i64,
        lease_secs: i64,
    ) -> Result<Option<Claim>> {
        anyhow::ensure!(
            !worker.is_empty() && (1..=86400).contains(&lease_secs),
            "invalid worker or lease"
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Recovery and claim share a transaction. An expired owner cannot commit later.
        tx.execute("UPDATE attempts SET status='interrupted', finished_at=?1, error='worker lease expired' WHERE id IN (SELECT active_attempt_id FROM tasks WHERE status='running' AND lease_until<=?1)", [now])?;
        tx.execute("UPDATE tasks SET status=CASE WHEN attempt_count<max_attempts THEN 'retry_scheduled' ELSE 'failed' END, available_at=?1, active_attempt_id=NULL, lease_until=NULL, last_error='worker lease expired', updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE status='running' AND lease_until<=?1", [now])?;
        let task = tx.query_row("SELECT * FROM tasks WHERE backend_id=?2 AND contract=?3 AND status IN ('queued','retry_scheduled') AND available_at<=?1 AND attempt_count<max_attempts ORDER BY id LIMIT 1", params![now,backend,contract.as_str()], task_from_row).optional()?;
        let Some(task) = task else {
            tx.commit()?;
            return Ok(None);
        };
        tx.execute(
            "INSERT INTO attempts(task_id,worker_id,status,started_at,backend_id,model,contract) VALUES(?1,?2,'running',?3,?4,?5,?6)",
            params![task.id, worker, now, backend, model, contract.as_str()],
        )?;
        let attempt_id = tx.last_insert_rowid();
        tx.execute("UPDATE tasks SET status='running', attempt_count=attempt_count+1, active_attempt_id=?2, lease_until=?3, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1", params![task.id,attempt_id,now+lease_secs])?;
        let task = get_task(&tx, task.id)?.context("claimed task missing")?;
        tx.commit()?;
        Ok(Some(Claim { task, attempt_id }))
    }

    pub fn heartbeat(&mut self, claim: &Claim, now: i64, lease_secs: i64) -> Result<bool> {
        Ok(self.connection.execute("UPDATE tasks SET lease_until=?3 WHERE id=?1 AND active_attempt_id=?2 AND status='running' AND lease_until>?4", params![claim.task.id,claim.attempt_id,now+lease_secs,now])? == 1)
    }

    pub fn attach_run(&mut self, claim: &Claim, dir: &str, now: i64) -> Result<()> {
        let changed = self.connection.execute("UPDATE attempts SET run_dir=?3 WHERE id=?2 AND task_id=?1 AND status='running' AND EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND active_attempt_id=?2 AND status='running' AND lease_until>?4)", params![claim.task.id,claim.attempt_id,dir,now])?;
        anyhow::ensure!(changed == 1, "task lease lost");
        Ok(())
    }

    pub fn succeed(
        &mut self,
        claim: &Claim,
        result: &AgentResult,
        session: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        result.validate().map_err(anyhow::Error::msg)?;
        let json = serde_json::to_string(result)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE tasks SET status='succeeded', result_json=?3, last_error=NULL, active_attempt_id=NULL, lease_until=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1 AND active_attempt_id=?2 AND status='running' AND lease_until>?4", params![claim.task.id,claim.attempt_id,json,now])?;
        if changed == 1 {
            tx.execute("UPDATE attempts SET status='succeeded', result_json=?2, session_id=?3, finished_at=?4 WHERE id=?1", params![claim.attempt_id,json,session,now])?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn fail(
        &mut self,
        claim: &Claim,
        error: &str,
        now: i64,
        delay_secs: i64,
        interrupted: bool,
    ) -> Result<bool> {
        anyhow::ensure!(delay_secs >= 0, "retry delay must not be negative");
        let error: String = error.chars().take(2000).collect();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE tasks SET status=CASE WHEN attempt_count<max_attempts THEN 'retry_scheduled' ELSE 'failed' END, available_at=?3, last_error=?4, active_attempt_id=NULL, lease_until=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1 AND active_attempt_id=?2 AND status='running' AND lease_until>?5", params![claim.task.id,claim.attempt_id,now+delay_secs,error,now])?;
        if changed == 1 {
            tx.execute(
                "UPDATE attempts SET status=?2, error=?3, finished_at=?4 WHERE id=?1",
                params![
                    claim.attempt_id,
                    if interrupted { "interrupted" } else { "failed" },
                    error,
                    now
                ],
            )?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }
}

fn task_from_row(row: &Row<'_>) -> rusqlite::Result<Task> {
    let raw_status: String = row.get(3)?;
    let status = raw_status.parse().map_err(|error: String| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        )
    })?;
    Ok(Task {
        id: row.get(0)?,
        request_key: row.get(1)?,
        backend_id: row.get("backend_id")?,
        contract: contract_column(row)?,
        prompt: row.get(2)?,
        status,
        created_at: row.get(4)?,
        updated_at: row.get("updated_at")?,
        attempt_count: row.get("attempt_count")?,
        max_attempts: row.get("max_attempts")?,
        active_attempt_id: row.get("active_attempt_id")?,
        lease_until: row.get("lease_until")?,
        available_at: row.get("available_at")?,
        result: json_column(row, "result_json")?,
        last_error: row.get("last_error")?,
    })
}

fn get_task(conn: &Connection, id: i64) -> Result<Option<Task>> {
    Ok(conn
        .query_row("SELECT * FROM tasks WHERE id=?1", [id], task_from_row)
        .optional()?)
}

fn json_column(row: &Row<'_>, name: &str) -> rusqlite::Result<Option<AgentResult>> {
    let raw: Option<String> = row.get(name)?;
    raw.map(|s| {
        serde_json::from_str(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
    })
    .transpose()
}

fn contract_column(row: &Row<'_>) -> rusqlite::Result<RunContract> {
    let raw: String = row.get("contract")?;
    raw.parse().map_err(|e: String| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })
}
