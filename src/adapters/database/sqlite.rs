mod consumers;
use super::TaskStatusCounts;
use crate::{
    domain::{AgentResult, Attempt, Claim, EnqueueResult, RunContract, Task},
    ports::storage::TaskStore,
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

/// 默认 `TaskStore` 适配器，基于单个 SQLite 数据库文件。
pub struct SqliteTaskStore {
    connection: Mutex<Connection>,
}

impl SqliteTaskStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).context("cannot create database directory")?;
        }
        let mut connection = Connection::open(path).context("cannot open task database")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        // 在读取版本号前，先与其他进程串行化启动迁移。
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        match version {
            0 => {
                tx.execute_batch(include_str!("../../../migrations/001_tasks.sql"))?;
                tx.execute_batch(include_str!("../../../migrations/002_execution.sql"))?;
            }
            1 => tx.execute_batch(include_str!("../../../migrations/002_execution.sql"))?,
            2..=7 => {}
            other => bail!("unsupported database version {other}; expected 7"),
        }
        if version < 3 {
            tx.execute_batch(include_str!("../../../migrations/003_backends.sql"))?;
        }
        if version < 4 {
            tx.execute_batch(include_str!("../../../migrations/004_notify.sql"))?;
        }
        if version < 5 {
            tx.execute_batch(include_str!("../../../migrations/005_records.sql"))?;
        }
        if version < 6 {
            tx.execute_batch(include_str!("../../../migrations/006_events.sql"))?;
        }
        if version < 7 {
            tx.execute_batch(include_str!("../../../migrations/007_consumers.sql"))?;
        }
        tx.commit()?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn task_status_counts(&self) -> Result<TaskStatusCounts> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT status, COUNT(*) FROM tasks GROUP BY status")?;
        let mut rows = statement.query([])?;
        let mut counts = TaskStatusCounts::default();
        while let Some(row) = rows.next()? {
            let status: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            counts.insert(&status, count)?;
        }
        Ok(counts)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("task store lock poisoned"))
    }
}

impl TaskStore for SqliteTaskStore {
    fn reply_interaction(&self, id: &str, response: &serde_json::Value, now: i64) -> Result<bool> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let body: Option<String> = tx
            .query_row(
                "SELECT body FROM records WHERE kind='interaction' AND id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(body) = body else {
            return Ok(false);
        };
        let mut request: crate::domain::records::Interaction = serde_json::from_str(&body)?;
        if request.status != "pending"
            || request.expires_at <= now
            || !request.options.contains(response)
        {
            return Ok(false);
        }
        let run_body: Option<String> = tx
            .query_row(
                "SELECT body FROM records WHERE kind='run' AND id=?1",
                [&request.run_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(run_body) = run_body else {
            return Ok(false);
        };
        let run: serde_json::Value = serde_json::from_str(&run_body)?;
        let (Some(task), Some(attempt)) = (run["task_id"].as_i64(), run["attempt_id"].as_i64())
        else {
            return Ok(false);
        };
        if run["status"] != "running" {
            return Ok(false);
        }
        let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND active_attempt_id=?2 AND status='running' AND lease_until>?3)", params![task,attempt,now], |r| r.get(0))?;
        if !active {
            return Ok(false);
        }
        request.status = "answered".into();
        request.response = Some(response.clone());
        tx.execute(
            "UPDATE records SET body=?2,revision=revision+1 WHERE kind='interaction' AND id=?1",
            params![id, serde_json::to_string(&request)?],
        )?;
        tx.commit()?;
        Ok(true)
    }

    fn document(&self, kind: &str, id: &str) -> Result<Option<crate::domain::records::Document>> {
        let conn = self.connection()?;
        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT revision,body FROM records WHERE kind=?1 AND id=?2",
                params![kind, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(revision, body)| {
            Ok(crate::domain::records::Document {
                id: id.into(),
                revision,
                value: serde_json::from_str(&body)?,
            })
        })
        .transpose()
    }
    fn documents(&self, kind: &str, limit: u32) -> Result<Vec<crate::domain::records::Document>> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare("SELECT id,revision,body FROM records WHERE kind=?1 ORDER BY id LIMIT ?2")?;
        let rows = stmt.query_map(params![kind, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|r| {
            let (id, revision, body) = r?;
            Ok(crate::domain::records::Document {
                id,
                revision,
                value: serde_json::from_str(&body)?,
            })
        })
        .collect()
    }
    fn put_document(
        &self,
        kind: &str,
        id: &str,
        expected: Option<i64>,
        value: &serde_json::Value,
    ) -> Result<bool> {
        let conn = self.connection()?;
        let body = serde_json::to_string(value)?;
        let n = if let Some(rev) = expected {
            conn.execute("UPDATE records SET body=?3,revision=revision+1 WHERE kind=?1 AND id=?2 AND revision=?4",params![kind,id,body,rev])?
        } else {
            conn.execute(
                "INSERT INTO records(kind,id,body) VALUES (?1,?2,?3) ON CONFLICT DO NOTHING",
                params![kind, id, body],
            )?
        };
        Ok(n == 1)
    }

    fn enqueue_for(
        &self,
        request_key: &str,
        prompt: &str,
        backend: &str,
        contract: RunContract,
    ) -> Result<EnqueueResult> {
        anyhow::ensure!(
            !backend.trim().is_empty() && backend.len() <= 128,
            "invalid backend ID"
        );
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
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

    fn get(&self, id: i64) -> Result<Option<Task>> {
        Ok(self
            .connection()?
            .query_row("SELECT * FROM tasks WHERE id = ?1", [id], task_from_row)
            .optional()?)
    }

    fn list(&self, limit: u32) -> Result<Vec<Task>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT * FROM tasks ORDER BY id DESC LIMIT ?1")?;
        let tasks = statement
            .query_map([limit], task_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tasks)
    }

    fn cancel(&self, id: i64) -> Result<Task> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
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

    fn retry(&self, id: i64) -> Result<Task> {
        let changed = self.connection()?.execute("UPDATE tasks SET status='queued', available_at=0, max_attempts=attempt_count+3, last_error=NULL, notified_at=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1 AND status='failed'", [id])?;
        anyhow::ensure!(changed == 1, "only a failed task can be retried");
        self.get(id)?.context("task not found")
    }

    fn attempts(&self, id: i64) -> Result<Vec<Attempt>> {
        self.get(id)?.context("task not found")?;
        let connection = self.connection()?;
        let mut q = connection.prepare("SELECT * FROM attempts WHERE task_id=?1 ORDER BY id")?;
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

    fn claim_for(
        &self,
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
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 恢复与认领共享同一事务。租约过期的原属主无法再提交。
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

    fn heartbeat(&self, claim: &Claim, now: i64, lease_secs: i64) -> Result<bool> {
        Ok(self.connection()?.execute("UPDATE tasks SET lease_until=?3 WHERE id=?1 AND active_attempt_id=?2 AND status='running' AND lease_until>?4", params![claim.task.id,claim.attempt_id,now+lease_secs,now])? == 1)
    }

    fn attach_run(&self, claim: &Claim, dir: &str, now: i64) -> Result<()> {
        let changed = self.connection()?.execute("UPDATE attempts SET run_dir=?3 WHERE id=?2 AND task_id=?1 AND status='running' AND EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND active_attempt_id=?2 AND status='running' AND lease_until>?4)", params![claim.task.id,claim.attempt_id,dir,now])?;
        anyhow::ensure!(changed == 1, "task lease lost");
        Ok(())
    }

    fn enqueue_with_notify(
        &self,
        request_key: &str,
        prompt: &str,
        backend: &str,
        contract: RunContract,
        notify: &crate::domain::NotifyRoute,
    ) -> Result<EnqueueResult> {
        anyhow::ensure!(
            !notify.channel.trim().is_empty() && notify.channel.len() <= 64,
            "invalid notify channel"
        );
        anyhow::ensure!(!notify.target.trim().is_empty(), "invalid notify target");
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "INSERT INTO tasks (request_key, prompt, backend_id, contract, notify_channel, notify_target, notify_msg_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(request_key) DO NOTHING",
            params![request_key, prompt, backend, contract.as_str(), notify.channel, notify.target, notify.msg_id],
        )?;
        let task = tx.query_row(
            "SELECT * FROM tasks WHERE request_key = ?1",
            [request_key],
            task_from_row,
        )?;
        if task.prompt != prompt
            || task.backend_id != backend
            || task.contract != contract
            || task.notify.as_ref() != Some(notify)
        {
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

    fn list_unnotified(&self, limit: u32) -> Result<Vec<Task>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT * FROM tasks WHERE notify_channel IS NOT NULL AND notified_at IS NULL AND status IN ('succeeded','failed') ORDER BY id LIMIT ?1",
        )?;
        let tasks = statement
            .query_map([limit], task_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tasks)
    }

    fn mark_notified(&self, id: i64) -> Result<bool> {
        Ok(self.connection()?.execute(
            "UPDATE tasks SET notified_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1 AND notified_at IS NULL",
            [id],
        )? == 1)
    }

    fn succeed(
        &self,
        claim: &Claim,
        result: &AgentResult,
        session: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        result.validate().map_err(anyhow::Error::msg)?;
        let json = serde_json::to_string(result)?;
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE tasks SET status='succeeded', result_json=?3, last_error=NULL, active_attempt_id=NULL, lease_until=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1 AND active_attempt_id=?2 AND status='running' AND lease_until>?4", params![claim.task.id,claim.attempt_id,json,now])?;
        if changed == 1 {
            tx.execute("UPDATE attempts SET status='succeeded', result_json=?2, session_id=?3, finished_at=?4 WHERE id=?1", params![claim.attempt_id,json,session,now])?;
        }
        tx.commit()?;
        Ok(changed == 1)
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
        anyhow::ensure!(delay_secs >= 0, "retry delay must not be negative");
        let error = crate::util::truncate_chars(error, crate::util::ERROR_MAX_CHARS);
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE tasks SET status=CASE WHEN ?6 AND attempt_count<max_attempts THEN 'retry_scheduled' ELSE 'failed' END, available_at=?3, last_error=?4, active_attempt_id=NULL, lease_until=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1 AND active_attempt_id=?2 AND status='running' AND lease_until>?5", params![claim.task.id,claim.attempt_id,now+delay_secs,error,now,retryable])?;
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
        notify: notify_column(row)?,
        notified_at: row.get("notified_at")?,
    })
}

fn notify_column(row: &Row<'_>) -> rusqlite::Result<Option<crate::domain::NotifyRoute>> {
    let channel: Option<String> = row.get("notify_channel")?;
    Ok(channel.map(|channel| crate::domain::NotifyRoute {
        channel,
        target: row.get("notify_target").unwrap_or_default(),
        msg_id: row.get("notify_msg_id").unwrap_or(None),
    }))
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

impl crate::ports::events::EventStore for SqliteTaskStore {
    fn events_after(
        &self,
        after: i64,
        limit: u32,
    ) -> Result<Vec<crate::domain::events::DomainEvent>> {
        anyhow::ensure!(
            after >= 0 && (1..=1000).contains(&limit),
            "invalid event cursor or limit"
        );
        let conn = self.connection()?;
        let mut query = conn.prepare("SELECT * FROM events WHERE id>?1 ORDER BY id LIMIT ?2")?;
        Ok(query
            .query_map(params![after, limit], |row| {
                Ok(crate::domain::events::DomainEvent {
                    id: row.get("id")?,
                    version: row.get("version")?,
                    kind: row.get("kind")?,
                    occurred_at: row.get("occurred_at")?,
                    resource_id: row.get("resource_id")?,
                    task_id: row.get("task_id")?,
                    attempt_id: row.get("attempt_id")?,
                    run_id: row.get("run_id")?,
                    status: row.get("status")?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    fn event_cursor(&self) -> Result<i64> {
        Ok(self
            .connection()?
            .query_row("SELECT COALESCE(MAX(id),0) FROM events", [], |r| r.get(0))?)
    }
}
