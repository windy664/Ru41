use crate::{
    app::store::TaskStore,
    domain::{AgentResult, Attempt, Claim, EnqueueResult, RunContract, Task, TaskStatus},
};
use anyhow::{Context, Result, bail};
use postgres::{Client, NoTls, Row};
use std::{
    mem::ManuallyDrop,
    sync::{Mutex, MutexGuard},
};

/// 迁移互斥用的事务级咨询锁键（ASCII "RU41"）。
const MIGRATION_LOCK: i64 = 0x5255_3431;

const TASK_COLUMNS: &str = "id, request_key, prompt, status, \
    to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS created_at, \
    to_char(updated_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS updated_at, \
    attempt_count, max_attempts, active_attempt_id, lease_until, available_at, \
    result_json, last_error, backend_id, contract, \
    notify_channel, notify_target, notify_msg_id, notified_at";

/// `TaskStore` 的 PostgreSQL 适配器，语义与 SQLite 实现一致。
pub struct PostgresTaskStore {
    client: Mutex<ManuallyDrop<Client>>,
}

/// postgres 同步客户端内部会 block_on；在 tokio runtime 中需先离开执行上下文。
fn outside_runtime<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(_) => tokio::task::block_in_place(f),
        Err(_) => f(),
    }
}

impl PostgresTaskStore {
    pub fn open(url: &str) -> Result<Self> {
        let mut client = Client::connect(url, NoTls).context("cannot connect to PostgreSQL")?;
        let mut tx = client.transaction()?;
        // 在读取版本号前，用事务级咨询锁与其他进程串行化启动迁移。
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&MIGRATION_LOCK])?;
        tx.batch_execute(
            "CREATE TABLE IF NOT EXISTS schema_migrations (\
                version INT PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT now())",
        )?;
        let version: Option<i32> = tx
            .query_one("SELECT max(version) FROM schema_migrations", &[])?
            .get(0);
        match version {
            None => {
                tx.batch_execute(include_str!("../../migrations/postgres/001_tasks.sql"))?;
                tx.execute("INSERT INTO schema_migrations(version) VALUES (1)", &[])?;
            }
            Some(1 | 2) => {}
            Some(other) => bail!("unsupported database version {other}; expected 2"),
        }
        if version.unwrap_or(0) < 2 {
            tx.batch_execute(include_str!("../../migrations/postgres/002_notify.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (2)", &[])?;
        }
        tx.commit()?;
        Ok(Self {
            client: Mutex::new(ManuallyDrop::new(client)),
        })
    }

    fn client(&self) -> Result<MutexGuard<'_, ManuallyDrop<Client>>> {
        self.client
            .lock()
            .map_err(|_| anyhow::anyhow!("task store lock poisoned"))
    }
}

impl Drop for PostgresTaskStore {
    fn drop(&mut self) {
        // Client 的 Drop 内部会 block_on；在 tokio runtime 里析构会 panic，
        // 此时放弃显式关闭，连接随进程退出由系统回收。
        if tokio::runtime::Handle::try_current().is_err()
            && let Ok(client) = self.client.get_mut()
        {
            unsafe { ManuallyDrop::drop(client) };
        }
    }
}

impl TaskStore for PostgresTaskStore {
    fn enqueue_for(
        &self,
        request_key: &str,
        prompt: &str,
        backend: &str,
        contract: RunContract,
    ) -> Result<EnqueueResult> {
        outside_runtime(|| {
            anyhow::ensure!(
                !backend.trim().is_empty() && backend.len() <= 128,
                "invalid backend ID"
            );
            let mut client = self.client()?;
            let mut tx = client.transaction()?;
            let changed = tx.execute(
                "INSERT INTO tasks (request_key, prompt, backend_id, contract) VALUES ($1, $2, $3, $4) ON CONFLICT(request_key) DO NOTHING",
                &[&request_key, &prompt, &backend, &contract.as_str()],
            )?;
            let task = task_from_row(&tx.query_one(
                &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE request_key = $1"),
                &[&request_key],
            )?)?;
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
        })
    }

    fn get(&self, id: i64) -> Result<Option<Task>> {
        outside_runtime(|| {
            self.client()?
                .query_opt(
                    &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = $1"),
                    &[&id],
                )?
                .map(|row| task_from_row(&row))
                .transpose()
        })
    }

    fn list(&self, limit: u32) -> Result<Vec<Task>> {
        outside_runtime(|| {
            self.client()?
                .query(
                    &format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY id DESC LIMIT $1"),
                    &[&(limit as i64)],
                )?
                .iter()
                .map(task_from_row)
                .collect()
        })
    }

    fn cancel(&self, id: i64) -> Result<Task> {
        outside_runtime(|| {
            let mut client = self.client()?;
            let mut tx = client.transaction()?;
            let task = get_task(&mut tx, id)?.context("task not found")?;
            if matches!(task.status, TaskStatus::Succeeded | TaskStatus::Failed) {
                bail!("terminal task cannot be cancelled");
            }
            tx.execute("UPDATE attempts SET status='cancelled', finished_at=extract(epoch from now())::bigint, error='cancelled by user' WHERE task_id=$1 AND status='running'", &[&id])?;
            tx.execute("UPDATE tasks SET status='cancelled', active_attempt_id=NULL, lease_until=NULL, updated_at=now() WHERE id=$1", &[&id])?;
            let task = get_task(&mut tx, id)?.context("task not found")?;
            tx.commit()?;
            Ok(task)
        })
    }

    fn retry(&self, id: i64) -> Result<Task> {
        outside_runtime(|| {
            let changed = self.client()?.execute("UPDATE tasks SET status='queued', available_at=0, max_attempts=attempt_count+3, last_error=NULL, updated_at=now() WHERE id=$1 AND status='failed'", &[&id])?;
            anyhow::ensure!(changed == 1, "only a failed task can be retried");
            self.get(id)?.context("task not found")
        })
    }

    fn attempts(&self, id: i64) -> Result<Vec<Attempt>> {
        outside_runtime(|| {
            self.get(id)?.context("task not found")?;
            self.client()?
                .query(
                    "SELECT id, task_id, worker_id, status, started_at, finished_at, run_dir, session_id, result_json, error, backend_id, model, contract FROM attempts WHERE task_id=$1 ORDER BY id",
                    &[&id],
                )?
                .iter()
                .map(attempt_from_row)
                .collect()
        })
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
        outside_runtime(|| {
            anyhow::ensure!(
                !worker.is_empty() && (1..=86400).contains(&lease_secs),
                "invalid worker or lease"
            );
            let mut client = self.client()?;
            let mut tx = client.transaction()?;
            // 恢复与认领共享同一事务。租约过期的原属主无法再提交。
            tx.execute("UPDATE attempts SET status='interrupted', finished_at=$1, error='worker lease expired' WHERE id IN (SELECT active_attempt_id FROM tasks WHERE status='running' AND lease_until<=$1)", &[&now])?;
            tx.execute("UPDATE tasks SET status=CASE WHEN attempt_count<max_attempts THEN 'retry_scheduled' ELSE 'failed' END, available_at=$1, active_attempt_id=NULL, lease_until=NULL, last_error='worker lease expired', updated_at=now() WHERE status='running' AND lease_until<=$1", &[&now])?;
            // SKIP LOCKED 让并发 Worker 各自领到不同任务。
            let task = tx
                .query_opt(
                    &format!(
                        "SELECT {TASK_COLUMNS} FROM tasks WHERE backend_id=$2 AND contract=$3 AND status IN ('queued','retry_scheduled') AND available_at<=$1 AND attempt_count<max_attempts ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED"
                    ),
                    &[&now, &backend, &contract.as_str()],
                )?
                .map(|row| task_from_row(&row))
                .transpose()?;
            let Some(task) = task else {
                tx.commit()?;
                return Ok(None);
            };
            let attempt_id: i64 = tx
                .query_one(
                    "INSERT INTO attempts(task_id,worker_id,status,started_at,backend_id,model,contract) VALUES($1,$2,'running',$3,$4,$5,$6) RETURNING id",
                    &[&task.id, &worker, &now, &backend, &model, &contract.as_str()],
                )?
                .get(0);
            tx.execute("UPDATE tasks SET status='running', attempt_count=attempt_count+1, active_attempt_id=$2, lease_until=$3, updated_at=now() WHERE id=$1", &[&task.id,&attempt_id,&(now+lease_secs)])?;
            let task = get_task(&mut tx, task.id)?.context("claimed task missing")?;
            tx.commit()?;
            Ok(Some(Claim { task, attempt_id }))
        })
    }

    fn heartbeat(&self, claim: &Claim, now: i64, lease_secs: i64) -> Result<bool> {
        outside_runtime(|| {
            Ok(self.client()?.execute("UPDATE tasks SET lease_until=$3 WHERE id=$1 AND active_attempt_id=$2 AND status='running' AND lease_until>$4", &[&claim.task.id,&claim.attempt_id,&(now+lease_secs),&now])? == 1)
        })
    }

    fn attach_run(&self, claim: &Claim, dir: &str, now: i64) -> Result<()> {
        outside_runtime(|| {
            let changed = self.client()?.execute("UPDATE attempts SET run_dir=$3 WHERE id=$2 AND task_id=$1 AND status='running' AND EXISTS(SELECT 1 FROM tasks WHERE id=$1 AND active_attempt_id=$2 AND status='running' AND lease_until>$4)", &[&claim.task.id,&claim.attempt_id,&dir,&now])?;
            anyhow::ensure!(changed == 1, "task lease lost");
            Ok(())
        })
    }

    fn enqueue_with_notify(
        &self,
        request_key: &str,
        prompt: &str,
        backend: &str,
        contract: RunContract,
        notify: &crate::domain::NotifyRoute,
    ) -> Result<EnqueueResult> {
        outside_runtime(|| {
            anyhow::ensure!(
                !notify.channel.trim().is_empty() && notify.channel.len() <= 64,
                "invalid notify channel"
            );
            anyhow::ensure!(!notify.target.trim().is_empty(), "invalid notify target");
            let mut client = self.client()?;
            let mut tx = client.transaction()?;
            let changed = tx.execute(
                "INSERT INTO tasks (request_key, prompt, backend_id, contract, notify_channel, notify_target, notify_msg_id) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT(request_key) DO NOTHING",
                &[&request_key, &prompt, &backend, &contract.as_str(), &notify.channel, &notify.target, &notify.msg_id],
            )?;
            let task = task_from_row(&tx.query_one(
                &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE request_key = $1"),
                &[&request_key],
            )?)?;
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
        })
    }

    fn list_unnotified(&self, limit: u32) -> Result<Vec<Task>> {
        outside_runtime(|| {
            self.client()?
                .query(
                    &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE notify_channel IS NOT NULL AND notified_at IS NULL AND status IN ('succeeded','failed') ORDER BY id LIMIT $1"),
                    &[&(limit as i64)],
                )?
                .iter()
                .map(task_from_row)
                .collect()
        })
    }

    fn mark_notified(&self, id: i64) -> Result<bool> {
        outside_runtime(|| {
            Ok(self.client()?.execute(
                "UPDATE tasks SET notified_at=now() WHERE id=$1 AND notified_at IS NULL",
                &[&id],
            )? == 1)
        })
    }

    fn succeed(
        &self,
        claim: &Claim,
        result: &AgentResult,
        session: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        outside_runtime(|| {
            result.validate().map_err(anyhow::Error::msg)?;
            let json = serde_json::to_string(result)?;
            let mut client = self.client()?;
            let mut tx = client.transaction()?;
            let changed = tx.execute("UPDATE tasks SET status='succeeded', result_json=$3, last_error=NULL, active_attempt_id=NULL, lease_until=NULL, updated_at=now() WHERE id=$1 AND active_attempt_id=$2 AND status='running' AND lease_until>$4", &[&claim.task.id,&claim.attempt_id,&json,&now])?;
            if changed == 1 {
                tx.execute("UPDATE attempts SET status='succeeded', result_json=$2, session_id=$3, finished_at=$4 WHERE id=$1", &[&claim.attempt_id,&json,&session,&now])?;
            }
            tx.commit()?;
            Ok(changed == 1)
        })
    }

    fn fail(
        &self,
        claim: &Claim,
        error: &str,
        now: i64,
        delay_secs: i64,
        interrupted: bool,
    ) -> Result<bool> {
        outside_runtime(|| {
            anyhow::ensure!(delay_secs >= 0, "retry delay must not be negative");
            let error: String = error.chars().take(2000).collect();
            let mut client = self.client()?;
            let mut tx = client.transaction()?;
            let changed = tx.execute("UPDATE tasks SET status=CASE WHEN attempt_count<max_attempts THEN 'retry_scheduled' ELSE 'failed' END, available_at=$3, last_error=$4, active_attempt_id=NULL, lease_until=NULL, updated_at=now() WHERE id=$1 AND active_attempt_id=$2 AND status='running' AND lease_until>$5", &[&claim.task.id,&claim.attempt_id,&(now+delay_secs),&error,&now])?;
            if changed == 1 {
                tx.execute(
                    "UPDATE attempts SET status=$2, error=$3, finished_at=$4 WHERE id=$1",
                    &[
                        &claim.attempt_id,
                        &(if interrupted { "interrupted" } else { "failed" }),
                        &error,
                        &now,
                    ],
                )?;
            }
            tx.commit()?;
            Ok(changed == 1)
        })
    }
}

fn task_from_row(row: &Row) -> Result<Task> {
    let raw_status: String = row.get("status");
    let status: TaskStatus = raw_status
        .parse()
        .map_err(|error: String| anyhow::anyhow!(error))?;
    let contract: RunContract = row
        .get::<_, String>("contract")
        .parse()
        .map_err(|error: String| anyhow::anyhow!(error))?;
    Ok(Task {
        id: row.get("id"),
        request_key: row.get("request_key"),
        backend_id: row.get("backend_id"),
        contract,
        prompt: row.get("prompt"),
        status,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        attempt_count: row.get("attempt_count"),
        max_attempts: row.get("max_attempts"),
        active_attempt_id: row.get("active_attempt_id"),
        lease_until: row.get("lease_until"),
        available_at: row.get("available_at"),
        result: json_column(row, "result_json")?,
        last_error: row.get("last_error"),
        notify: row
            .get::<_, Option<String>>("notify_channel")
            .map(|channel| crate::domain::NotifyRoute {
                channel,
                target: row.get("notify_target"),
                msg_id: row.get("notify_msg_id"),
            }),
        notified_at: row.get("notified_at"),
    })
}

fn attempt_from_row(row: &Row) -> Result<Attempt> {
    let contract: RunContract = row
        .get::<_, String>("contract")
        .parse()
        .map_err(|error: String| anyhow::anyhow!(error))?;
    Ok(Attempt {
        id: row.get("id"),
        task_id: row.get("task_id"),
        worker_id: row.get("worker_id"),
        status: row.get("status"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
        run_dir: row.get("run_dir"),
        session_id: row.get("session_id"),
        backend_id: row.get("backend_id"),
        model: row.get("model"),
        contract,
        result: json_column(row, "result_json")?,
        error: row.get("error"),
    })
}

fn get_task(tx: &mut postgres::Transaction<'_>, id: i64) -> Result<Option<Task>> {
    tx.query_opt(
        &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id=$1"),
        &[&id],
    )?
    .map(|row| task_from_row(&row))
    .transpose()
}

fn json_column(row: &Row, name: &str) -> Result<Option<AgentResult>> {
    let raw: Option<String> = row.get(name);
    raw.map(|s| serde_json::from_str(&s).map_err(anyhow::Error::from))
        .transpose()
}
