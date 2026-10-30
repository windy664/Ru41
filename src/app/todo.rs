use crate::{
    domain::{
        NotifyRoute,
        records::Document,
        todo::{
            TODO_KIND, TodoInput, TodoItem, TodoOptions, TodoOverview, TodoPriority, TodoStatus,
            TodoView, due_label, local_date,
        },
    },
    ports::{channel::NotifyChannel, storage::TaskStore},
};
use anyhow::{Context, Result, ensure};

/// 到点提醒的单轮发送：汇总应提醒条目，经通知渠道送达后成功才去重；
/// 发送失败不记录，由调度循环下一轮重试。
pub async fn send_due_reminders(
    service: &TodoService,
    notifier: &dyn NotifyChannel,
    route: &NotifyRoute,
    options: TodoOptions,
) -> Result<serde_json::Value> {
    let now = crate::util::now();
    let due = service.due_reminders(now, 20)?;
    if due.is_empty() {
        return Ok(serde_json::json!({"due": 0, "sent": false}));
    }
    notifier
        .send(&route.target, None, &format_reminder(&due, now, options))
        .await?;
    // 只有通知成功后才记录，失败会在下一轮重试；极端崩溃窗口下可能重发一条。
    for item in &due {
        service.mark_reminded(&item.id, now)?;
    }
    Ok(serde_json::json!({"due": due.len(), "sent": true}))
}

/// 主动提醒的 QQ markdown 文本；与 `/待办` 列表保持一致。
pub fn format_reminder(items: &[TodoItem], now: i64, options: TodoOptions) -> String {
    let mut lines = vec![format!("## 📋 待办提醒（{}）", items.len())];
    for item in items {
        lines.push(format!(
            "- **{}** — {}",
            item.title,
            due_label(item.due_at, now, options, true)
        ));
    }
    lines.join("\n")
}

/// 私人常驻待办用例；与一次性 Agent Task 不同，Todo 关注长期承诺、截止和逾期。
#[derive(Clone)]
pub struct TodoService {
    store: std::sync::Arc<dyn TaskStore>,
    options: TodoOptions,
}

impl TodoService {
    pub fn new(store: std::sync::Arc<dyn TaskStore>, options: TodoOptions) -> Self {
        Self { store, options }
    }

    pub fn options(&self) -> TodoOptions {
        self.options
    }

    pub fn store(&self) -> &std::sync::Arc<dyn TaskStore> {
        &self.store
    }

    pub fn add(&self, input: TodoInput, now: i64) -> Result<TodoItem> {
        let mut labels = Vec::new();
        for label in input.labels {
            let label = label.trim().to_owned();
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        let item = TodoItem {
            id: new_todo_id(now),
            title: input.title.trim().to_owned(),
            status: TodoStatus::Active,
            priority: input.priority,
            labels,
            due_at: input.due_at,
            remind_at: input.remind_at,
            last_reminded_at: None,
            completed_at: None,
            created_at: now,
            updated_at: now,
            source: input.source,
        };
        item.validate()?;
        self.store
            .put_document(TODO_KIND, &item.id, None, &serde_json::to_value(&item)?)?;
        Ok(item)
    }

    pub fn list(&self, view: TodoView, now: i64, limit: usize) -> Result<Vec<TodoItem>> {
        ensure!(
            (1..=1000).contains(&limit),
            "todo list limit must be 1..1000"
        );
        let mut items = Vec::new();
        for (_, item) in self.entries()? {
            // 平台作业不进个人待办视图，作业走 /作业 聚合与平台自有提醒。
            if item.is_homework() {
                continue;
            }
            let include = match view {
                TodoView::All => true,
                TodoView::Today => self.bucket(&item, now) == Some(TodoBucket::Today),
                TodoView::Recent => self.bucket(&item, now) == Some(TodoBucket::Recent),
                TodoView::LongTerm => self.bucket(&item, now) == Some(TodoBucket::LongTerm),
                TodoView::Overdue => self.bucket(&item, now) == Some(TodoBucket::Overdue),
                TodoView::Inbox => self.bucket(&item, now) == Some(TodoBucket::Inbox),
            };
            if include {
                items.push(item);
            }
        }
        sort_active(&mut items);
        items.truncate(limit);
        Ok(items)
    }

    /// 今日、近期、长期、收件箱和逾期在读取时计算，不把会过期的分类写进数据。
    pub fn overview(&self, now: i64, limit: usize) -> Result<TodoOverview> {
        let mut overview = TodoOverview::default();
        for (_, item) in self.entries()? {
            // 与 list 一致：平台作业不进个人待办概览。
            if item.is_homework() {
                continue;
            }
            match self.bucket(&item, now) {
                Some(TodoBucket::Overdue) => overview.overdue.push(item),
                Some(TodoBucket::Today) => overview.today.push(item),
                Some(TodoBucket::Recent) => overview.recent.push(item),
                Some(TodoBucket::LongTerm) => overview.long_term.push(item),
                Some(TodoBucket::Inbox) => overview.inbox.push(item),
                None => {}
            }
        }
        for items in [
            &mut overview.overdue,
            &mut overview.today,
            &mut overview.recent,
            &mut overview.long_term,
            &mut overview.inbox,
        ] {
            sort_active(items);
            items.truncate(limit);
        }
        Ok(overview)
    }

    pub fn complete(&self, selector: &str, now: i64) -> Result<TodoItem> {
        self.update(selector, now, |item| {
            ensure!(item.status == TodoStatus::Active, "todo item is not active");
            item.status = TodoStatus::Completed;
            item.completed_at = Some(now);
            Ok(())
        })
    }

    pub fn cancel(&self, selector: &str, now: i64) -> Result<TodoItem> {
        self.update(selector, now, |item| {
            ensure!(item.status == TodoStatus::Active, "todo item is not active");
            item.status = TodoStatus::Cancelled;
            Ok(())
        })
    }

    pub fn restore(&self, selector: &str, now: i64) -> Result<TodoItem> {
        self.update(selector, now, |item| {
            ensure!(
                matches!(item.status, TodoStatus::Completed | TodoStatus::Cancelled),
                "todo item is already active"
            );
            item.status = TodoStatus::Active;
            item.completed_at = None;
            Ok(())
        })
    }

    pub fn postpone(&self, selector: &str, due_text: &str, now: i64) -> Result<TodoItem> {
        let due_at = crate::domain::todo::parse_due(due_text, now, &self.options)?;
        self.update(selector, now, |item| {
            ensure!(item.status == TodoStatus::Active, "todo item is not active");
            item.due_at = Some(due_at);
            item.remind_at = Some(due_at);
            item.last_reminded_at = None;
            Ok(())
        })
    }

    /// 已到提醒时间且本提醒尚未送达的活跃事项。
    pub fn due_reminders(&self, now: i64, limit: usize) -> Result<Vec<TodoItem>> {
        ensure!(
            (1..=200).contains(&limit),
            "todo reminder limit must be 1..200"
        );
        let mut items: Vec<TodoItem> = self
            .entries()?
            .into_iter()
            .map(|(_, item)| item)
            .filter(|item| {
                // 平台作业的临期提醒由各自平台服务负责，todo-remind 只管个人待办。
                !item.is_homework()
                    && item.status == TodoStatus::Active
                    && item.remind_at.is_some_and(|remind_at| remind_at <= now)
                    && item.last_reminded_at.unwrap_or(0) < item.remind_at.unwrap_or(0)
            })
            .collect();
        items.sort_by(|a, b| {
            a.remind_at
                .unwrap_or(i64::MAX)
                .cmp(&b.remind_at.unwrap_or(i64::MAX))
                .then_with(|| a.id.cmp(&b.id))
        });
        items.truncate(limit);
        Ok(items)
    }

    /// 同一提醒必须先送达成功再调用；发送失败时保留状态供下一轮重试。
    pub fn mark_reminded(&self, id: &str, now: i64) -> Result<()> {
        self.update_exact(id, now, |item| {
            item.last_reminded_at = Some(now);
            Ok(())
        })?;
        Ok(())
    }

    fn entries(&self) -> Result<Vec<(Document, TodoItem)>> {
        let mut entries = Vec::new();
        for document in self.store.documents(TODO_KIND, 10_000)? {
            let item: TodoItem = serde_json::from_value(document.value.clone())
                .with_context(|| format!("invalid todo item {}", document.id))?;
            item.validate()?;
            entries.push((document, item));
        }
        Ok(entries)
    }

    fn bucket(&self, item: &TodoItem, now: i64) -> Option<TodoBucket> {
        if item.status != TodoStatus::Active {
            return None;
        }
        let Some(due_at) = item.due_at else {
            return Some(TodoBucket::Inbox);
        };
        if due_at < now {
            return Some(TodoBucket::Overdue);
        }
        let today = local_date(now, self.options.offset_seconds);
        if local_date(due_at, self.options.offset_seconds) == today {
            return Some(TodoBucket::Today);
        }
        if due_at <= now + self.options.recent_days * 86_400 {
            return Some(TodoBucket::Recent);
        }
        Some(TodoBucket::LongTerm)
    }

    fn update_exact(
        &self,
        id: &str,
        now: i64,
        mutate: impl FnOnce(&mut TodoItem) -> Result<()>,
    ) -> Result<TodoItem> {
        let Some((document, mut item)) = self
            .entries()?
            .into_iter()
            .find(|(document, _)| document.id == id)
        else {
            anyhow::bail!("todo item not found: {id}");
        };
        mutate(&mut item)?;
        item.updated_at = now;
        item.validate()?;
        ensure!(
            self.store.put_document(
                TODO_KIND,
                &document.id,
                Some(document.revision),
                &serde_json::to_value(&item)?,
            )?,
            "todo item changed concurrently"
        );
        Ok(item)
    }

    fn update(
        &self,
        selector: &str,
        now: i64,
        mutate: impl FnOnce(&mut TodoItem) -> Result<()>,
    ) -> Result<TodoItem> {
        let selector = selector.trim();
        ensure!(!selector.is_empty(), "todo selector is required");
        let matches: Vec<(Document, TodoItem)> = self
            .entries()?
            .into_iter()
            .filter(|(_, item)| selector_matches(item, selector))
            .collect();
        ensure!(!matches.is_empty(), "todo item not found: {selector}");
        ensure!(
            matches.len() == 1,
            "todo selector is ambiguous: {selector} matches {} items",
            matches.len()
        );
        let (document, mut item) = matches.into_iter().next().expect("checked one match");
        mutate(&mut item)?;
        item.updated_at = now;
        item.validate()?;
        ensure!(
            self.store.put_document(
                TODO_KIND,
                &document.id,
                Some(document.revision),
                &serde_json::to_value(&item)?,
            )?,
            "todo item changed concurrently"
        );
        Ok(item)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TodoBucket {
    Overdue,
    Today,
    Recent,
    LongTerm,
    Inbox,
}

fn selector_matches(item: &TodoItem, selector: &str) -> bool {
    item.id == selector || item.short_id() == selector || item.short_id().starts_with(selector)
}

fn sort_active(items: &mut [TodoItem]) {
    items.sort_by(|a, b| {
        a.due_at
            .unwrap_or(i64::MAX)
            .cmp(&b.due_at.unwrap_or(i64::MAX))
            .then_with(|| b.priority.cmp(&a.priority))
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn new_todo_id(now: i64) -> String {
    format!("todo_{now:010}_{:016x}", rand::random::<u64>())
}

#[allow(dead_code)]
fn priority_is_used(_: TodoPriority) {}
