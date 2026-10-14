use super::*;
use crate::{
    domain::{consumers::*, events::DomainEvent},
    ports::consumers::ConsumerStore,
};
use anyhow::ensure;

impl ConsumerStore for SqliteTaskStore {
    fn register_consumer(&self, spec: &ConsumerSpec) -> Result<ConsumerState> {
        spec.validate()?;
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest: i64 =
            tx.query_row("SELECT COALESCE(MAX(id),0) FROM events", [], |r| r.get(0))?;
        ensure!(
            spec.start_after <= latest,
            "consumer cursor is ahead of event log"
        );
        tx.execute("INSERT INTO event_consumers(name,start_after,cursor,max_attempts) VALUES(?1,?2,?2,?3) ON CONFLICT(name) DO NOTHING",params![spec.name,spec.start_after,spec.max_attempts])?;
        let state = tx.query_row(
            "SELECT * FROM event_consumers WHERE name=?1",
            [&spec.name],
            state_row,
        )?;
        ensure!(
            state.start_after == spec.start_after && state.max_attempts == spec.max_attempts,
            "consumer configuration conflict"
        );
        tx.commit()?;
        Ok(state)
    }
    fn consumer(&self, name: &str) -> Result<Option<ConsumerState>> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT * FROM event_consumers WHERE name=?1",
                [name],
                state_row,
            )
            .optional()?)
    }
    fn claim_event(&self, name: &str, now: i64, lease_secs: i64) -> Result<Option<EventDelivery>> {
        let deadline = lease_deadline(now, lease_secs)?;
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = tx
            .query_row(
                "SELECT * FROM event_consumers WHERE name=?1",
                [name],
                state_row,
            )
            .optional()?
            .context("consumer not registered")?;
        if state.blocked
            || state.available_at > now
            || state.lease_until.is_some_and(|end| end > now)
        {
            return Ok(None);
        }
        if state.attempts >= state.max_attempts {
            tx.execute("UPDATE event_consumers SET blocked=TRUE,lease_until=NULL,last_failure='lease_expired' WHERE name=?1",[name])?;
            tx.commit()?;
            return Ok(None);
        }
        let event = tx
            .query_row(
                "SELECT * FROM events WHERE id=?2 OR (?2 IS NULL AND id>?1) ORDER BY id LIMIT 1",
                params![state.cursor, state.active_event_id],
                event_row,
            )
            .optional()?;
        let Some(event) = event else { return Ok(None) };
        tx.execute("UPDATE event_consumers SET active_event_id=?2,lease_until=?3,generation=generation+1,attempts=attempts+1 WHERE name=?1", params![name,event.id,deadline])?;
        tx.commit()?;
        Ok(Some(EventDelivery {
            consumer: name.into(),
            event,
            generation: state.generation + 1,
            attempt: state.attempts + 1,
        }))
    }
    fn renew_event(&self, d: &EventDelivery, now: i64, lease_secs: i64) -> Result<bool> {
        let deadline = lease_deadline(now, lease_secs)?;
        Ok(self.connection()?.execute("UPDATE event_consumers SET lease_until=?5 WHERE name=?1 AND active_event_id=?2 AND generation=?3 AND lease_until>?4 AND blocked=FALSE",params![d.consumer,d.event.id,d.generation,now,deadline])? == 1)
    }
    fn complete_event(&self, d: &EventDelivery, now: i64) -> Result<bool> {
        ensure!(now >= 0, "invalid timestamp");
        Ok(self.connection()?.execute("UPDATE event_consumers SET cursor=active_event_id,active_event_id=NULL,lease_until=NULL,attempts=0,available_at=0,last_failure=NULL WHERE name=?1 AND active_event_id=?2 AND generation=?3 AND lease_until>?4 AND blocked=FALSE", params![d.consumer,d.event.id,d.generation,now])? == 1)
    }
    fn fail_event(
        &self,
        d: &EventDelivery,
        now: i64,
        retry_at: i64,
        failure: DeliveryFailure,
    ) -> Result<bool> {
        ensure!(
            now >= 0 && retry_at >= now && retry_at - now <= 86400,
            "invalid retry time"
        );
        Ok(self.connection()?.execute("UPDATE event_consumers SET lease_until=NULL,available_at=?5,blocked=(attempts>=max_attempts),last_failure=?6 WHERE name=?1 AND active_event_id=?2 AND generation=?3 AND lease_until>?4 AND blocked=FALSE",params![d.consumer,d.event.id,d.generation,now,retry_at,failure.as_str()])? == 1)
    }
    fn retry_consumer(&self, name: &str, now: i64) -> Result<bool> {
        ensure!(now >= 0, "invalid timestamp");
        Ok(self.connection()?.execute("UPDATE event_consumers SET blocked=FALSE,attempts=0,available_at=?2,lease_until=NULL,generation=generation+1,last_failure=NULL WHERE name=?1 AND blocked=TRUE",params![name,now])? == 1)
    }
}

fn state_row(row: &Row<'_>) -> rusqlite::Result<ConsumerState> {
    Ok(ConsumerState {
        name: row.get("name")?,
        start_after: row.get("start_after")?,
        cursor: row.get("cursor")?,
        max_attempts: row.get("max_attempts")?,
        attempts: row.get("attempts")?,
        generation: row.get("generation")?,
        active_event_id: row.get("active_event_id")?,
        lease_until: row.get("lease_until")?,
        available_at: row.get("available_at")?,
        blocked: row.get("blocked")?,
        last_failure: row.get("last_failure")?,
    })
}
fn event_row(row: &Row<'_>) -> rusqlite::Result<DomainEvent> {
    Ok(DomainEvent {
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
}
