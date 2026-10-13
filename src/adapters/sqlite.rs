use crate::domain::{EnqueueResult, Task};
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
        connection.busy_timeout(Duration::from_secs(5))?;
        // Serialize startup migration with other processes before reading the version.
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        match version {
            0 => tx.execute_batch(include_str!("../../migrations/001_tasks.sql"))?,
            1 => {}
            other => bail!("unsupported database version {other}; expected 1"),
        }
        tx.commit()?;
        Ok(Self { connection })
    }

    pub fn enqueue(&mut self, request_key: &str, prompt: &str) -> Result<EnqueueResult> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "INSERT INTO tasks (request_key, prompt) VALUES (?1, ?2) ON CONFLICT(request_key) DO NOTHING",
            params![request_key, prompt],
        )?;
        let task = tx.query_row(
            "SELECT id, request_key, prompt, status, created_at, updated_at FROM tasks WHERE request_key = ?1",
            [request_key],
            task_from_row,
        )?;
        if task.prompt != prompt {
            bail!("request key already belongs to a different prompt; use a new key");
        }
        tx.commit()?;
        Ok(EnqueueResult {
            created: changed == 1,
            task,
        })
    }

    pub fn get(&self, id: i64) -> Result<Option<Task>> {
        Ok(self.connection.query_row(
            "SELECT id, request_key, prompt, status, created_at, updated_at FROM tasks WHERE id = ?1",
            [id],
            task_from_row,
        ).optional()?)
    }

    pub fn list(&self, limit: u32) -> Result<Vec<Task>> {
        let mut statement = self.connection.prepare(
            "SELECT id, request_key, prompt, status, created_at, updated_at FROM tasks ORDER BY id DESC LIMIT ?1",
        )?;
        let tasks = statement
            .query_map([limit], task_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tasks)
    }

    pub fn cancel(&mut self, id: i64) -> Result<Task> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE tasks SET status = 'cancelled', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1 AND status = 'queued'",
            [id],
        )?;
        let task = tx.query_row(
            "SELECT id, request_key, prompt, status, created_at, updated_at FROM tasks WHERE id = ?1",
            [id],
            task_from_row,
        ).optional()?.context("task not found")?;
        tx.commit()?;
        Ok(task)
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
        prompt: row.get(2)?,
        status,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}
