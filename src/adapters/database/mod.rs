pub mod postgres;
pub mod sqlite;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct TaskStatusCounts {
    pub queued: u64,
    pub running: u64,
    pub retry_scheduled: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub cancelled: u64,
}

impl TaskStatusCounts {
    fn insert(&mut self, status: &str, count: i64) -> anyhow::Result<()> {
        let status = status
            .parse::<crate::domain::TaskStatus>()
            .map_err(anyhow::Error::msg)?;
        let count = u64::try_from(count)?;
        match status {
            crate::domain::TaskStatus::Queued => self.queued = count,
            crate::domain::TaskStatus::Running => self.running = count,
            crate::domain::TaskStatus::RetryScheduled => self.retry_scheduled = count,
            crate::domain::TaskStatus::Succeeded => self.succeeded = count,
            crate::domain::TaskStatus::Failed => self.failed = count,
            crate::domain::TaskStatus::Cancelled => self.cancelled = count,
        }
        Ok(())
    }
}
