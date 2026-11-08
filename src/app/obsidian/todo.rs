//! 待办视图双向同步：todo 数据库 → `04-Life-生活/Todo-待办.md` 每轮重渲；
//! 文件里的勾选变更与手写新行回写数据库。

use super::ObsidianService;
use anyhow::Result;

/// 待办视图在 vault 里的位置（与数据库双向同步：勾选状态会写回）。
const TODO_VIEW_PATH: &str = "04-Life-生活/Todo-待办.md";

/// 渲染闸门的持久化戳：上次渲染时的数据版本 / 本地日期 / 文件内容 hash。
const STAMP_KIND: &str = "obsidian_todo_view";
const STAMP_ID: &str = "stamp";

/// 内容指纹（DefaultHasher 进程内稳定即可，不持久化跨进程比对需求之外的值）。
fn content_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// 闸门判定：数据库版本没变、没跨天（截止分节随日期翻滚）、
/// 视图文件没被人手动改过（勾选回写依赖发现文件差异）——三者皆新鲜才跳过。
fn view_is_fresh(
    stamp: Option<&serde_json::Value>,
    version: u64,
    today: &str,
    existing_hash: Option<u64>,
) -> bool {
    let Some(stamp) = stamp else {
        return false;
    };
    stamp["version"].as_u64() == Some(version)
        && stamp["date"].as_str() == Some(today)
        && existing_hash.is_some_and(|hash| stamp["hash"].as_u64() == Some(hash))
}

impl ObsidianService {
    /// 待办视图：todo 数据库 ↔ `04-Life-生活/Todo-待办.md` 双向同步。
    /// 数据库 → 文件：每轮重渲（内容没变不写）；文件 → 数据库：先读旧文件里的
    /// 勾选状态写回（打勾=完成、取消勾=恢复），再按最新状态渲染。
    /// 变更以 [ru41] 前缀自提交（下一轮游标自认，不会触发整理）。
    ///
    /// 渲染闸门（2026-11-08）：数据库版本/日期/文件 hash 都没变就整轮跳过——
    /// 原来每 5 分钟两次 list(1000) + 全量重渲，空闲时全是空转。
    pub(super) async fn render_todo_view(&self) -> Result<()> {
        let Some(options) = self.todo_options else {
            return Ok(());
        };
        let service = crate::app::todo::TodoService::new(self.store.clone(), options);
        let now = crate::util::now();
        let vault = self.vault.clone();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            // 闸门：一次文件读 + 两个索引文档读，替代原来的两次全表 list。
            let existing = vault.read_file(TODO_VIEW_PATH)?;
            let version = service.data_version()?;
            let today = crate::domain::todo::local_date(now, options.offset_seconds);
            let stamp = store.document(STAMP_KIND, STAMP_ID)?;
            let fresh = view_is_fresh(
                stamp.as_ref().map(|doc| &doc.value),
                version,
                &today,
                existing.as_deref().map(content_hash),
            );
            if fresh {
                return Ok(());
            }
            if let Some(existing) = existing.as_deref() {
                // 1. 勾选回写：旧文件里用户手动的 [x]/[ ] 变更写回数据库。
                //    先取当前状态对比，只在勾选与数据库不一致时调用，
                //    否则每轮都会对同一批条目空调 complete/restore 刷错误日志。
                let current: std::collections::HashMap<String, crate::domain::todo::TodoStatus> =
                    service
                        .list(crate::domain::todo::TodoView::All, now, 1000)?
                        .into_iter()
                        .map(|item| (item.id, item.status))
                        .collect();
                for (id, checked) in parse_checkbox_states(existing) {
                    let needs_sync = match current.get(&id) {
                        Some(crate::domain::todo::TodoStatus::Completed) => !checked,
                        Some(_) => checked,
                        None => false, // id 已失效（被删/重建），忽略
                    };
                    if !needs_sync {
                        continue;
                    }
                    let result = if checked {
                        service.complete(&id, now).map(|_| ())
                    } else {
                        service.restore(&id, now).map(|_| ())
                    };
                    if let Err(error) = result {
                        // 状态已一致或 id 已失效：忽略单项，不挡整轮。
                        eprintln!("[obsidian] todo checkbox sync {id}: {error:#}");
                    }
                }
                // 2. 新增回写：无 `#id` 锚点的 `- [ ]` 行是 Obsidian 里手写的新待办。
                let known_titles: std::collections::HashSet<String> = service
                    .list(crate::domain::todo::TodoView::All, now, 1000)?
                    .into_iter()
                    .filter(|item| item.status == crate::domain::todo::TodoStatus::Active)
                    .map(|item| item.title)
                    .collect();
                for (title, due_text) in parse_new_task_lines(existing) {
                    if known_titles.contains(&title) {
                        continue; // 同名活跃待办已存在，不重复建
                    }
                    let due_at = due_text
                        .as_deref()
                        .and_then(|text| crate::domain::todo::parse_due(text, now, &options).ok());
                    let input = crate::domain::todo::TodoInput {
                        title: title.clone(),
                        priority: crate::domain::todo::TodoPriority::Normal,
                        labels: vec![],
                        due_at,
                        remind_at: due_at,
                        source: Some(crate::domain::todo::TodoSource {
                            channel: "obsidian".into(),
                            source_id: None,
                        }),
                    };
                    if let Err(error) = service.add(input, now) {
                        eprintln!("[obsidian] todo add from view {title:?}: {error:#}");
                    }
                }
            }
            // 3. 按最新数据库状态渲染。
            let overview = service.overview(now, 50)?;
            let completed_recent: Vec<crate::domain::todo::TodoItem> = service
                .list(crate::domain::todo::TodoView::All, now, 1000)?
                .into_iter()
                .filter(|item| {
                    item.status == crate::domain::todo::TodoStatus::Completed
                        && item.completed_at.is_some_and(|at| at >= now - 7 * 86_400)
                })
                .collect();
            let content = render_todo_markdown(&overview, &completed_recent, now, options);
            if vault.read_file(TODO_VIEW_PATH)?.as_deref() != Some(content.as_str()) {
                vault.write_file(TODO_VIEW_PATH, &content)?;
                if vault.commit_all("[ru41] todo 视图同步")? {
                    vault.push()?;
                }
            }
            // 刷新闸门戳：本次完整 pass 后文件内容 == content。
            // 失败只记日志，下轮多渲染一次兜底。
            let revision = store
                .document(STAMP_KIND, STAMP_ID)?
                .map(|doc| doc.revision);
            if let Err(error) = store.put_document(
                STAMP_KIND,
                STAMP_ID,
                revision,
                &serde_json::json!({
                    "version": version,
                    "date": today,
                    "hash": content_hash(&content),
                }),
            ) {
                eprintln!("[obsidian] todo view stamp write failed: {error:#}");
            }
            Ok(())
        })
        .await??;
        Ok(())
    }
}

/// 从视图文件解析勾选状态：`- [ ]`/`- [x]` 行尾的 `` `#id` `` → (id, 是否勾选)。
fn parse_checkbox_states(content: &str) -> Vec<(String, bool)> {
    let mut states = Vec::new();
    for line in content.lines() {
        let line = line.trim_start();
        // "- [ ]" 剥掉 "- [" 后剩 " ] ..."（前导空格）；"- [x]" 剩 "x] ..."。
        let checked = match line.strip_prefix("- [") {
            Some(rest) if rest.starts_with(" ] ") => false,
            Some(rest) if rest.starts_with("x] ") || rest.starts_with("X] ") => true,
            _ => continue,
        };
        // 行尾 `` `#id` `` 标记
        let Some(start) = line.rfind("`#") else {
            continue;
        };
        let id = &line[start + 2..];
        let Some(end) = id.find('`') else {
            continue;
        };
        let id = id[..end].trim();
        if !id.is_empty() {
            states.push((id.to_owned(), checked));
        }
    }
    states
}

/// 解析视图里用户手写的新待办行：`- [ ] <标题>` 且无 `` `#id` `` 锚点。
/// 支持 Obsidian Tasks 的 `📅 YYYY-MM-DD` 截止标记（从标题剔除，单独返回）。
fn parse_new_task_lines(content: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim_start();
        let Some(text) = line.strip_prefix("- [ ] ") else {
            continue;
        };
        if text.contains("`#") {
            continue; // 有锚点的是数据库渲染行，不是新增
        }
        let (title, due) = match text.find("📅 ") {
            Some(pos) => {
                let due = text[pos + "📅 ".len()..]
                    .split_whitespace()
                    .next()
                    .map(str::to_owned);
                (text[..pos].trim().to_owned(), due)
            }
            None => (text.trim().to_owned(), None),
        };
        if !title.is_empty() {
            out.push((title, due));
        }
    }
    out
}

/// 分节：已过期/今天/近期/长期/未排期 + 近 7 天已完成。
/// 待办概况渲染成 Obsidian 任务清单：严格 `- [ ]` 复选格式，行尾 `` `#id` `` 供回写。
fn render_todo_markdown(
    overview: &crate::domain::todo::TodoOverview,
    completed_recent: &[crate::domain::todo::TodoItem],
    now: i64,
    options: crate::domain::todo::TodoOptions,
) -> String {
    use crate::domain::todo::due_label;
    let mut out = String::from(
        "# 待办清单\n\n\
         > Ru41 与 Obsidian 双向同步：打勾 = 完成、取消勾 = 恢复（写回数据库）。\n\
         > 新增：直接写一行 `- [ ] 标题`（支持 `📅 YYYY-MM-DD` 截止），下一轮同步自动入库并分配锚点。\n\
         > 修改/删除走 QQ `/待办` 或 CLI `ru41 todo`；带锚点的正文行会被覆盖，勾选状态保留。\n",
    );
    let sections: [(&str, &Vec<crate::domain::todo::TodoItem>, bool); 5] = [
        ("已过期", &overview.overdue, true),
        ("今天", &overview.today, false),
        ("近期", &overview.recent, false),
        ("长期", &overview.long_term, false),
        ("未排期", &overview.inbox, false),
    ];
    for (title, items, overdue) in sections {
        if items.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {title}\n"));
        for item in items {
            let due = due_label(item.due_at, now, options, overdue);
            out.push_str(&format!("- [ ] {}（{}）`#{}`\n", item.title, due, item.id));
        }
    }
    if !completed_recent.is_empty() {
        out.push_str("\n## 已完成（近 7 天）\n");
        for item in completed_recent {
            let done = item
                .completed_at
                .map(|at| crate::domain::todo::local_date(at, options.offset_seconds))
                .unwrap_or_default();
            out.push_str(&format!(
                "- [x] {}（完成于 {}）`#{}`\n",
                item.title, done, item.id
            ));
        }
    }
    if overview.active_count() == 0 && completed_recent.is_empty() {
        out.push_str("\n（空）\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_view_renders_sections_and_empty() {
        let options = crate::domain::todo::TodoOptions {
            offset_seconds: 8 * 3600,
            recent_days: 7,
            long_term_days: 30,
        };
        let empty = crate::domain::todo::TodoOverview::default();
        assert!(render_todo_markdown(&empty, &[], 0, options).contains("（空）"));

        let mut overview = crate::domain::todo::TodoOverview::default();
        overview.today.push(crate::domain::todo::TodoItem {
            id: "abc".into(),
            title: "交实验报告".into(),
            status: crate::domain::todo::TodoStatus::Active,
            priority: crate::domain::todo::TodoPriority::Normal,
            labels: vec![],
            due_at: None,
            remind_at: None,
            last_reminded_at: None,
            completed_at: None,
            created_at: 0,
            updated_at: 0,
            source: None,
        });
        let md = render_todo_markdown(&overview, &[], 0, options);
        assert!(md.contains("## 今天"));
        assert!(md.contains("- [ ] 交实验报告"));
        assert!(!md.contains("## 已过期"), "空分节不出现");
    }

    #[test]
    fn parses_new_task_lines_without_anchor() {
        let content = "- [ ] 手写的新待办\n\
             - [ ] 带截止的新待办 📅 2026-10-31\n\
             - [ ] 数据库渲染行（有锚点）`#todo_1_abc`\n\
             - [x] 已完成的不算新增\n\
             - [ ] \n";
        let new_tasks = parse_new_task_lines(content);
        assert_eq!(
            new_tasks,
            vec![
                ("手写的新待办".to_owned(), None),
                ("带截止的新待办".to_owned(), Some("2026-10-31".to_owned())),
            ]
        );
    }

    #[test]
    fn view_freshness_gate() {
        let stamp = serde_json::json!({"version": 7, "date": "2026-11-08", "hash": 42});
        // 三要素全对上才新鲜。
        assert!(view_is_fresh(Some(&stamp), 7, "2026-11-08", Some(42)));
        // 数据库有变更 → 重渲。
        assert!(!view_is_fresh(Some(&stamp), 8, "2026-11-08", Some(42)));
        // 跨天（截止分节翻滚）→ 重渲。
        assert!(!view_is_fresh(Some(&stamp), 7, "2026-11-09", Some(42)));
        // 文件被人动过（勾选回写窗口）→ 重渲。
        assert!(!view_is_fresh(Some(&stamp), 7, "2026-11-08", Some(43)));
        // 文件缺失或戳缺失 → 重渲。
        assert!(!view_is_fresh(Some(&stamp), 7, "2026-11-08", None));
        assert!(!view_is_fresh(None, 7, "2026-11-08", Some(42)));
    }

    #[test]
    fn parses_checkbox_states_from_view() {
        let content = "# 待办清单\n\n## 今天\n- [ ] 交报告（今天）`#todo_1_abc`\n\
             - [x] 已做完的事（完成于 2026-10-25）`#todo_2_def`\n\
             - [X] 大写也算 `#todo_3_ghi`\n\
             - 普通列表不是任务 `#todo_4_jkl`\n\
             - [ ] 没有 id 标记的行\n";
        let states = parse_checkbox_states(content);
        assert_eq!(
            states,
            vec![
                ("todo_1_abc".to_owned(), false),
                ("todo_2_def".to_owned(), true),
                ("todo_3_ghi".to_owned(), true),
            ]
        );
    }
}
