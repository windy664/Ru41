//! 待办视图双向同步：todo 数据库 → `04-Life-生活/Todo-待办.md` 每轮重渲。
//! 数据库是权威存储；文件回写走差量——只有相对「上次渲染快照」被用户
//! 改过的勾选/标题才写回数据库，没动过的行以数据库为准（QQ 侧 /完成
//! 不会再被旧文件顶回去）。删行不删库，防止误清空。

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
    /// 数据库 → 文件：每轮重渲（内容没变不写）；文件 → 数据库：差量回写——
    /// 只把「相对上次渲染快照」的勾选翻转与标题改名写回（打勾=完成、取消勾=恢复），
    /// 用户没碰过的行不参与对拍，数据库状态原样渲染。无快照（首发/清戳恢复）
    /// 时整轮只渲染不回写，数据库全量说了算。
    /// 变更以 [ru41] 前缀自提交（下一轮游标自认，不会触发整理）。
    ///
    /// 渲染闸门（2026-11-08）：数据库版本/日期/文件 hash 都没变就整轮跳过——
    /// 原来每 5 分钟两次 list(1000) + 全量重渲，空闲时全是空转。
    /// 快照回写（2026-11-09）：闸门戳里多存一份上次渲染正文，回写只认差量。
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
                let current: std::collections::HashMap<String, crate::domain::todo::TodoItem> =
                    service
                        .list(crate::domain::todo::TodoView::All, now, 1000)?
                        .into_iter()
                        .map(|item| (item.id.clone(), item))
                        .collect();
                // 1. 勾选与标题差量回写：只对相对快照被用户翻转/改名的行动手。
                //    没有快照（首发或清戳恢复）时跳过整段回写，避免把数据库状态
                //    按旧文件「恢复」回去。
                if let Some(snapshot) = stamp.as_ref().and_then(|doc| doc.value["content"].as_str())
                {
                    for (id, checked) in checkbox_flips(existing, snapshot) {
                        let Some(item) = current.get(&id) else {
                            continue; // id 已失效（被删/重建），忽略
                        };
                        let needs_sync = match item.status {
                            crate::domain::todo::TodoStatus::Completed => !checked,
                            _ => checked,
                        };
                        if !needs_sync {
                            continue; // 状态已一致（如对同一行重复翻转），不空刷
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
                    for (id, middle) in renamed_lines(existing, snapshot) {
                        let Some(item) = current.get(&id) else {
                            continue;
                        };
                        if let Some(title) = edited_title(&middle, item, now, options)
                            && let Err(error) = service.rename(&id, &title, now)
                        {
                            eprintln!("[obsidian] todo rename sync {id}: {error:#}");
                        }
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
            // 刷新闸门戳：本次完整 pass 后文件内容 == content；正文一并存为
            // 下轮回写的比对快照。
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
                    "content": content,
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

/// 从视图文件解析带锚点的渲染行：`- [ ]`/`- [x]` 开头、行尾 `` `#id` ``
/// → (id, 是否勾选, 勾选框与锚点之间的正文)。正文用于发现标题改名。
fn parse_anchored_lines(content: &str) -> Vec<(String, bool, String)> {
    let mut lines = Vec::new();
    for line in content.lines() {
        let line = line.trim_start();
        // "- [ ]" 剥掉 "- [" 后剩 " ] ..."（前导空格）；"- [x]" 剩 "x] ..."。
        let (checked, rest) = match line.strip_prefix("- [") {
            Some(rest) if rest.starts_with(" ] ") => (false, &rest[3..]),
            Some(rest) if rest.starts_with("x] ") || rest.starts_with("X] ") => (true, &rest[3..]),
            _ => continue,
        };
        // 行尾 `` `#id` `` 标记
        let Some(start) = rest.rfind("`#") else {
            continue;
        };
        let id = &rest[start + 2..];
        let Some(end) = id.find('`') else {
            continue;
        };
        let id = id[..end].trim();
        if !id.is_empty() {
            lines.push((id.to_owned(), checked, rest[..start].trim_end().to_owned()));
        }
    }
    lines
}

/// 勾选翻转检测：只认「相对上次渲染快照」的翻转，返回 (id, 现在是否勾选)。
/// 没动过的行不出现在结果里——数据库侧的变更（QQ /完成等）因此不会被旧文件顶回。
/// 快照里没有的带锚点行（用户自己贴了带锚点的行）按用户意图处理。
fn checkbox_flips(existing: &str, snapshot: &str) -> Vec<(String, bool)> {
    let snap: std::collections::HashMap<String, bool> = parse_anchored_lines(snapshot)
        .into_iter()
        .map(|(id, checked, _)| (id, checked))
        .collect();
    parse_anchored_lines(existing)
        .into_iter()
        .filter(|(id, checked, _)| {
            snap.get(id)
                .is_none_or(|snap_checked| snap_checked != checked)
        })
        .map(|(id, checked, _)| (id, checked))
        .collect()
}

/// 改名检测：正文（勾选框与锚点之间）相对快照不一致才算用户改名，
/// 返回 (id, 现有正文)。数据库侧状态变化导致的后缀翻滚（未排期 → 完成于…）
/// 在渲染层发生、不进文件比对，不会误判。
fn renamed_lines(existing: &str, snapshot: &str) -> Vec<(String, String)> {
    let snap: std::collections::HashMap<String, String> = parse_anchored_lines(snapshot)
        .into_iter()
        .map(|(id, _, middle)| (id, middle))
        .collect();
    parse_anchored_lines(existing)
        .into_iter()
        .filter(|(id, _, middle)| {
            snap.get(id)
                .is_some_and(|snap_middle| snap_middle != middle)
        })
        .map(|(id, _, middle)| (id, middle))
        .collect()
}

/// 从锚点行正文反解用户改过的标题：剥掉渲染时追加的「（截止）」/「（完成于 日期）」
/// 后缀后与数据库标题比对，不同且非空即视为改名。后缀没被保留时整段正文都是标题。
fn edited_title(
    middle: &str,
    item: &crate::domain::todo::TodoItem,
    now: i64,
    options: crate::domain::todo::TodoOptions,
) -> Option<String> {
    use crate::domain::todo::due_label;
    let mut title = middle.trim().to_owned();
    let mut suffixes = vec![
        format!("（{}）", due_label(item.due_at, now, options, true)),
        format!("（{}）", due_label(item.due_at, now, options, false)),
    ];
    if let Some(done) = item
        .completed_at
        .map(|at| crate::domain::todo::local_date(at, options.offset_seconds))
    {
        suffixes.push(format!("（完成于 {done}）"));
    }
    for suffix in suffixes {
        if let Some(stripped) = title.strip_suffix(suffix.as_str()) {
            title = stripped.trim_end().to_owned();
            break;
        }
    }
    if title.is_empty() || title == item.title {
        return None;
    }
    Some(title)
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
    let mut out = String::from("# 待办清单\n");
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
    fn checkbox_flips_only_counts_user_edits_against_snapshot() {
        let snapshot = "- [ ] 甲（未排期）`#a`\n- [ ] 乙（未排期）`#b`\n";
        // 用户勾了甲 → 只有甲出现在翻转列表。
        let existing = "- [x] 甲（未排期）`#a`\n- [ ] 乙（未排期）`#b`\n";
        assert_eq!(
            checkbox_flips(existing, snapshot),
            vec![("a".to_owned(), true)]
        );
        // 数据库侧完成了甲但文件没动 → 没有翻转，数据库为准不回写。
        assert!(checkbox_flips(snapshot, snapshot).is_empty());
        // 快照里没有的带锚点行按用户意图处理。
        let pasted = "- [x] 丙（未排期）`#c`\n";
        assert_eq!(
            checkbox_flips(pasted, snapshot),
            vec![("c".to_owned(), true)]
        );
    }

    #[test]
    fn renamed_lines_only_counts_text_edits_against_snapshot() {
        let snapshot = "- [ ] 甲（未排期）`#a`\n- [ ] 乙（未排期）`#b`\n";
        // 用户改了甲的标题 → 识别；乙没动 → 不参与。
        let existing = "- [ ] 甲改（未排期）`#a`\n- [ ] 乙（未排期）`#b`\n";
        assert_eq!(
            renamed_lines(existing, snapshot),
            vec![("a".to_owned(), "甲改（未排期）".to_owned())]
        );
        // 只翻勾选不改正文 → 不算改名。
        let flipped = "- [x] 甲（未排期）`#a`\n- [ ] 乙（未排期）`#b`\n";
        assert!(renamed_lines(flipped, snapshot).is_empty());
    }

    #[test]
    fn parses_anchored_lines_with_middle_text() {
        let content = "# 待办清单\n\n## 今天\n- [ ] 交报告（今天）`#todo_1_abc`\n\
             - [x] 已做完的事（完成于 2026-10-25）`#todo_2_def`\n\
             - [X] 大写也算 `#todo_3_ghi`\n\
             - 普通列表不是任务 `#todo_4_jkl`\n\
             - [ ] 没有 id 标记的行\n";
        let lines = parse_anchored_lines(content);
        assert_eq!(
            lines,
            vec![
                ("todo_1_abc".to_owned(), false, "交报告（今天）".to_owned()),
                (
                    "todo_2_def".to_owned(),
                    true,
                    "已做完的事（完成于 2026-10-25）".to_owned()
                ),
                ("todo_3_ghi".to_owned(), true, "大写也算".to_owned()),
            ]
        );
    }

    #[test]
    fn edited_title_detects_rename_and_keeps_untouched() {
        let options = crate::domain::todo::TodoOptions {
            offset_seconds: 8 * 3600,
            recent_days: 7,
            long_term_days: 30,
        };
        let item = crate::domain::todo::TodoItem {
            id: "todo_1_abc".into(),
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
        };
        // 原样渲染行：无改名。
        assert_eq!(
            edited_title("交实验报告（未排期）", &item, 0, options),
            None
        );
        // 改了标题、保留后缀：识别出改名。
        assert_eq!(
            edited_title("交实验报告和心得（未排期）", &item, 0, options),
            Some("交实验报告和心得".to_owned())
        );
        // 连后缀一起改：整段正文作为新标题。
        assert_eq!(
            edited_title("交实验报告（周四前）", &item, 0, options),
            Some("交实验报告（周四前）".to_owned())
        );
        // 只留后缀或空标题：拒绝改名。
        assert_eq!(edited_title("（未排期）", &item, 0, options), None);
        assert_eq!(edited_title("", &item, 0, options), None);
    }
}
