//! 行程服务：事件的增删查、周期展开、占用冲突检测与到点提醒。
//! 与 TodoService 并列的核心服务；QQ 命令、CLI 与 todo-remind 循环共用。

use std::sync::Arc;

use anyhow::{Context, Result, ensure};

use crate::{
    domain::{
        NotifyRoute,
        records::Document,
        schedule::{
            EVENT_KIND, EventInput, EventItem, EventOccurrence, EventStatus, REMIND_LEAD_SECS,
            event_time_label,
        },
        todo::TodoOptions,
    },
    ports::{channel::NotifyChannel, storage::TaskStore},
};

pub struct ScheduleService {
    store: Arc<dyn TaskStore>,
    options: TodoOptions,
}

impl ScheduleService {
    pub fn new(store: Arc<dyn TaskStore>, options: TodoOptions) -> Self {
        Self { store, options }
    }

    pub fn add(&self, input: EventInput, now: i64) -> Result<EventItem> {
        let item = EventItem {
            id: new_event_id(now),
            title: input.title.trim().to_owned(),
            status: EventStatus::Active,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            location: input
                .location
                .map(|l| l.trim().to_owned())
                .filter(|l| !l.is_empty()),
            recurrence: input.recurrence,
            reminded_starts: Vec::new(),
            created_at: now,
            updated_at: now,
            source: input.source,
        };
        item.validate()?;
        self.put(&item, None)?;
        Ok(item)
    }

    /// 按短 id（或完整 id）取消事件；取消后不再展开发生与提醒。
    pub fn cancel(&self, selector: &str, now: i64) -> Result<EventItem> {
        let (document, mut item) = self
            .find(selector)?
            .with_context(|| format!("event {selector} not found"))?;
        ensure!(
            item.status == EventStatus::Active,
            "event {} is already cancelled",
            item.short_id()
        );
        item.status = EventStatus::Cancelled;
        item.updated_at = now;
        self.put(&item, Some(document.revision))?;
        Ok(item)
    }

    /// 未来 horizon 天内的发生，按开始时间升序。
    pub fn upcoming(
        &self,
        now: i64,
        horizon_days: i64,
        limit: usize,
    ) -> Result<Vec<EventOccurrence>> {
        self.occurrences_between(now, now + horizon_days * 86400, limit)
    }

    /// 今天本地日内的发生（供 /待办 今日视图合并）。
    pub fn today(&self, now: i64) -> Result<Vec<EventOccurrence>> {
        let start = now - self.options.offset_seconds as i64;
        let day_start = start - start.rem_euclid(86400) + self.options.offset_seconds as i64;
        self.occurrences_between(day_start, day_start + 86400, 50)
    }

    /// 展开 [window_start, window_end) 内全部激活事件的发生。
    pub fn occurrences_between(
        &self,
        window_start: i64,
        window_end: i64,
        limit: usize,
    ) -> Result<Vec<EventOccurrence>> {
        let mut out = Vec::new();
        for (_, item) in self.entries()? {
            if item.status != EventStatus::Active {
                continue;
            }
            out.extend(item.occurrences_between(
                window_start,
                window_end,
                self.options.offset_seconds,
            ));
            if out.len() >= limit {
                break;
            }
        }
        out.sort_by_key(|o| o.starts_at);
        out.truncate(limit);
        Ok(out)
    }

    /// 占用冲突：与 [starts_at, ends_at) 重叠的未来发生，供 agent 规划行程前查询。
    pub fn conflicts(
        &self,
        starts_at: i64,
        ends_at: i64,
        now: i64,
    ) -> Result<Vec<EventOccurrence>> {
        ensure!(ends_at > starts_at, "conflict window must be non-empty");
        Ok(self
            .occurrences_between(starts_at, ends_at.max(now + 86400), 100)?
            .into_iter()
            .filter(|o| o.starts_at < ends_at && o.ends_at > starts_at)
            .collect())
    }

    /// 到点提醒：开始时刻落在 [now, now + lead] 且尚未结束、未提醒过的发生。
    pub fn due_event_reminders(
        &self,
        now: i64,
        lead: i64,
        limit: usize,
    ) -> Result<Vec<EventOccurrence>> {
        let mut due = Vec::new();
        for (_, item) in self.entries()? {
            if item.status != EventStatus::Active {
                continue;
            }
            for occurrence in
                item.occurrences_between(now - 86400, now + lead, self.options.offset_seconds)
            {
                if occurrence.starts_at <= now + lead
                    && occurrence.ends_at > now
                    && !item.reminded(occurrence.starts_at)
                {
                    due.push(occurrence);
                }
            }
        }
        due.sort_by_key(|o| o.starts_at);
        due.truncate(limit);
        Ok(due)
    }

    /// 通知成功后记录已提醒；乐观并发，更新失败留给下一轮重试（可能重发一条）。
    pub fn mark_reminded(&self, occurrence: &EventOccurrence, now: i64) -> Result<()> {
        let (document, mut item) = self
            .find(&occurrence.event_id)?
            .with_context(|| format!("event {} not found", occurrence.event_id))?;
        item.mark_reminded(occurrence.starts_at);
        item.updated_at = now;
        ensure!(
            self.put(&item, Some(document.revision))?,
            "event {} updated concurrently",
            item.short_id()
        );
        Ok(())
    }

    fn entries(&self) -> Result<Vec<(Document, EventItem)>> {
        let mut entries = Vec::new();
        for document in self.store.documents(EVENT_KIND, 10_000)? {
            let item: EventItem = serde_json::from_value(document.value.clone())
                .with_context(|| format!("invalid event {}", document.id))?;
            item.validate()?;
            entries.push((document, item));
        }
        Ok(entries)
    }

    fn find(&self, selector: &str) -> Result<Option<(Document, EventItem)>> {
        for (document, item) in self.entries()? {
            let matched = if selector.starts_with("evt_") {
                item.id == selector
            } else {
                item.short_id() == selector
            };
            if matched {
                return Ok(Some((document, item)));
            }
        }
        Ok(None)
    }

    fn put(&self, item: &EventItem, expected: Option<i64>) -> Result<bool> {
        self.store
            .put_document(EVENT_KIND, &item.id, expected, &serde_json::to_value(item)?)
    }
}

/// 行程到点提醒的 QQ markdown 文本；与待办提醒同一风格。
pub fn format_event_reminder(items: &[EventOccurrence], options: TodoOptions) -> String {
    let mut lines = vec![format!("## 行程提醒（{}）", items.len())];
    for item in items {
        let mut line = format!(
            "- **{}** — {}",
            item.title,
            event_time_label(item.starts_at, item.recurring, options.offset_seconds)
        );
        if let Some(location) = &item.location {
            line.push_str(&format!(" @ {location}"));
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// 发送到点行程提醒；发送成功才记录，失败留给下一轮重试。
pub async fn send_event_reminders(
    service: &ScheduleService,
    notifier: &dyn NotifyChannel,
    route: &NotifyRoute,
    options: TodoOptions,
) -> Result<serde_json::Value> {
    let now = crate::util::now();
    let due = service.due_event_reminders(now, REMIND_LEAD_SECS, 20)?;
    if due.is_empty() {
        return Ok(serde_json::json!({"due": 0, "sent": false}));
    }
    notifier
        .send(&route.target, None, &format_event_reminder(&due, options))
        .await?;
    for occurrence in &due {
        service.mark_reminded(occurrence, now)?;
    }
    Ok(serde_json::json!({"due": due.len(), "sent": true}))
}

fn new_event_id(now: i64) -> String {
    format!("evt_{now:010}_{:016x}", rand::random::<u64>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminder_label_includes_location() {
        let options = TodoOptions::default();
        let occurrence = EventOccurrence {
            event_id: "evt_x".into(),
            title: "例会".into(),
            location: Some("线上".into()),
            starts_at: 1_800_000_000,
            ends_at: 1_800_003_600,
            recurring: false,
        };
        let text = format_event_reminder(&[occurrence], options);
        assert!(text.contains("例会"));
        assert!(text.contains("@ 线上"));
    }
}
