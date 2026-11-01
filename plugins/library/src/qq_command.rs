use anyhow::Result;
use ru41::ports::commands::{CommandContext, CommandHandler, MarkupButton, Reply, ReplyMarkup};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
};

use crate::service::{format_loan_reply, local_today};
use crate::source::{LibraryClient, LibraryWatch, LoanRecord};

/// QQ 借阅/续借命令：`/借阅`（在借清单 + 续借按钮）、`/借阅 历史`、
/// `/续借 书名`。个人信息，双闸（身份绑定 + 仅单聊）。
pub struct LibraryQqCommand {
    client: Arc<LibraryClient>,
}

impl LibraryQqCommand {
    pub fn new(client: Arc<LibraryClient>) -> Self {
        Self { client }
    }
}

impl CommandHandler for LibraryQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(verb, "借阅" | "借书" | "图书" | "续借" | "找书" | "查书" | "馆藏" | "追书" | "订书")
    }

    fn help(&self) -> &'static str {
        "`/借阅`：在借清单（可点续借按钮）；`/续借 书名`：续借；`/找书 关键词`：查馆藏（索书号/在架）；`/追书 书名`：馆藏可借到货提醒"
    }

    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(reply) = ctx.require_private("查询借阅", "借阅记录属于个人数据")? {
                return Ok(Some(Reply::text(reply)));
            }
            let trimmed = arg.trim();
            if trimmed.is_empty() || trimmed == "在借" {
                self.list_loans().await
            } else if trimmed == "历史" {
                self.list_history().await
            } else {
                // 「/借阅 xxx」不拦截书名查询以外的东西：交给 Agent。
                Ok(Some(Reply::text(
                    "用法：`/借阅` 查看在借清单；`/借阅 历史` 查看最近借阅；`/续借 书名` 续借；`/找书 关键词` 查馆藏。",
                )))
            }
        })
    }

    fn handle_command<'a>(
        &'a self,
        verb: &'a str,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        match verb {
            "续借" => Box::pin(async move {
                if let Some(reply) = ctx.require_private("续借图书", "借阅记录属于个人数据")? {
                    return Ok(Some(Reply::text(reply)));
                }
                self.renew(arg.trim()).await
            }),
            // 找书是公开馆藏检索（无个人数据），无需私聊闸。
            "找书" | "查书" | "馆藏" => Box::pin(async move { self.search(arg.trim()).await }),
            // 追书是个人跟踪清单（含推送），与借阅同级私聊闸。
            "追书" | "订书" => Box::pin(async move {
                if let Some(reply) = ctx.require_private("追书", "追书清单属于个人数据")? {
                    return Ok(Some(Reply::text(reply)));
                }
                self.watch_cmd(arg.trim()).await
            }),
            _ => self.handle(arg, ctx),
        }
    }
}

impl LibraryQqCommand {
    async fn list_loans(&self) -> Result<Option<Reply>> {
        if let Err(err) = self.client.ensure_session().await {
            return Ok(Some(Reply::text(format!(
                "## 图书馆登录失败\n\n{err}\n\n稍后会自动重试；若持续失败请检查统一身份认证密码。"
            ))));
        }
        let page = match self.client.loans().await {
            Ok(page) => page,
            Err(err) => {
                return Ok(Some(Reply::text(format!("## 借阅查询失败\n\n{err}"))));
            }
        };
        if page.records.is_empty() {
            return Ok(Some(Reply::text(format!(
                "## 在借清单\n\n当前没有在借图书 🎉\n\n- 可借额度：{} 册",
                page.loan_num.map_or("--".into(), |n| n.to_string())
            ))));
        }
        let today = local_today();
        let mut lines = vec![format!(
            "## 在借清单（{} 本）\n",
            page.records.len()
        )];
        for record in &page.records {
            let days = record
                .norm_return_date
                .as_deref()
                .and_then(|d| {
                    chrono::NaiveDate::parse_from_str(
                        d.trim().get(..10).unwrap_or(d),
                        "%Y-%m-%d",
                    )
                    .ok()
                })
                .map(|due| (due - today).num_days());
            match days {
                Some(days) => lines.push(crate::service::format_loan_line(record, days)),
                None => lines.push(format!(
                    "- 《{}》｜应还日期未知",
                    record.display_title()
                )),
            }
        }
        lines.push(String::new());
        lines.push("> 点下方按钮直接续借，或回复 `/续借 书名`".into());
        let text = lines.join("\n");
        let markup = format_loan_reply(&page);
        Ok(Some(Reply::with_markup(text, markup)))
    }

    async fn list_history(&self) -> Result<Option<Reply>> {
        if let Err(err) = self.client.ensure_session().await {
            return Ok(Some(Reply::text(format!(
                "## 图书馆登录失败\n\n{err}"
            ))));
        }
        let page = match self.client.loan_history(10).await {
            Ok(page) => page,
            Err(err) => {
                return Ok(Some(Reply::text(format!("## 借阅历史查询失败\n\n{err}"))));
            }
        };
        if page.records.is_empty() {
            return Ok(Some(Reply::text("## 借阅历史\n\n还没有借阅记录。".to_owned())));
        }
        let mut lines = vec!["## 借阅历史（最近 10 条）\n".to_owned()];
        for record in &page.records {
            let returned = record
                .return_date
                .clone()
                .unwrap_or_else(|| "在借".into());
            lines.push(format!(
                "- 《{}》｜借出 {}｜归还 {}",
                record.display_title(),
                record.loan_date.as_deref().unwrap_or("?"),
                returned
            ));
        }
        Ok(Some(Reply::text(lines.join("\n"))))
    }

    /// `/找书 关键词`：公开馆藏检索（免登录），附前几条的在架状态。
    async fn search(&self, query: &str) -> Result<Option<Reply>> {
        if query.is_empty() {
            return Ok(Some(Reply::text(
                "用法：`/找书 关键词`（书名/作者/主题词均可）".to_owned(),
            )));
        }
        let page = match self.client.search_books(query, 1, 5).await {
            Ok(page) => page,
            Err(err) => {
                return Ok(Some(Reply::text(format!("## 馆藏检索失败\n\n{err}"))));
            }
        };
        if page.books.is_empty() {
            return Ok(Some(Reply::text(format!(
                "## 馆藏检索\n\n「{query}」没有命中馆藏。\n\n> 换个关键词试试（可用更泛的词，如「深度学习」而非全名）。"
            ))));
        }
        let mut lines = vec![format!("## 找书「{query}」（共 {} 条）\n", page.num_found)];
        let top = page.books.len().min(3);
        for (index, book) in page.books.iter().enumerate() {
            let meta = [
                book.author.as_deref().unwrap_or(""),
                book.publisher.as_deref().unwrap_or(""),
                book.publish_year.as_deref().unwrap_or(""),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("｜");
            let call_no = book.call_no.as_deref().unwrap_or("无索书号");
            lines.push(format!(
                "{}. 《{}》\n　{}｜索书号 {}",
                index + 1,
                book.title,
                if meta.is_empty() { "未知" } else { &meta },
                call_no
            ));
            // 前 3 条附在架状态。
            if index < top
                && let Ok(items) = self.client.holdings(book.record_id).await
            {
                if items.is_empty() {
                    lines.push("　↳ 无单册馆藏记录".into());
                } else {
                    // 样本库仅供阅览不算可借；按馆藏地分组展示在架情况。
                    let loanable = items
                        .iter()
                        .filter(|h| h.is_on_shelf() && !h.is_sample())
                        .count();
                    let mut order: Vec<String> = Vec::new();
                    let mut parts: Vec<(String, usize, usize, bool)> = Vec::new();
                    for item in &items {
                        let loc = item.location_name.clone().unwrap_or_else(|| "未知".into());
                        let sample = item.is_sample();
                        let idx = order
                            .iter()
                            .position(|l| *l == loc)
                            .unwrap_or_else(|| {
                                order.push(loc.clone());
                                parts.push((loc, 0, 0, sample));
                                parts.len() - 1
                            });
                        parts[idx].2 += 1;
                        if item.is_on_shelf() {
                            parts[idx].1 += 1;
                        }
                    }
                    let detail = parts
                        .iter()
                        .map(|(loc, on, total, sample)| {
                            if *sample {
                                format!("{loc} {total}（仅供阅览）")
                            } else {
                                format!("{loc} 在架{on}/{total}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("、");
                    lines.push(format!("　↳ 可借 {loanable} 册｜{detail}"));
                }
            }
        }
        if page.num_found > 5 {
            lines.push(format!(
                "\n> 仅显示前 5 条；回复更精确的关键词缩小范围，或上 findgzus.libsp.cn 翻页。"
            ));
        }
        // 前 3 条给追书按钮（点了就是发 /追书 <recordId>）。
        let mut markup = ReplyMarkup::new();
        for book in page.books.iter().take(3) {
            let short: String = book.title.chars().take(12).collect();
            markup.push_row(vec![MarkupButton::new(
                format!("📌追书 {short}"),
                format!("/追书 {}", book.record_id),
            )]);
        }
        Ok(Some(Reply::with_markup(lines.join("\n"), markup)))
    }

    /// `/追书`：跟踪清单管理。无参/列表=查看；`取消 <id或书名>`；数字=按 recordId
    /// 直接跟踪；其他=先检索再由按钮选定（避免追错书）。
    async fn watch_cmd(&self, query: &str) -> Result<Option<Reply>> {
        if query.is_empty() || query == "列表" {
            return self.list_watches().await;
        }
        if let Some(rest) = query.strip_prefix("取消") {
            return self.cancel_watch(rest.trim());
        }
        if let Ok(record_id) = query.parse::<i64>() {
            return self.add_watch_by_id(record_id).await;
        }
        // 关键词：检索 → 唯一命中直接加，多条出按钮。
        let page = match self.client.search_books(query, 1, 5).await {
            Ok(page) => page,
            Err(err) => return Ok(Some(Reply::text(format!("## 馆藏检索失败\n\n{err}")))),
        };
        match page.books.len() {
            0 => Ok(Some(Reply::text(format!(
                "## 追书失败\n\n「{query}」没有命中馆藏，请换个关键词。"
            )))),
            1 => self.add_watch_by_id(page.books[0].record_id).await,
            _ => {
                let mut lines = vec![format!("## 选一本跟踪（共 {} 条命中）\n", page.num_found)];
                let mut markup = ReplyMarkup::new();
                for (index, book) in page.books.iter().enumerate() {
                    lines.push(format!(
                        "{}. 《{}》｜{}",
                        index + 1,
                        book.title,
                        book.call_no.as_deref().unwrap_or("无索书号")
                    ));
                    let short: String = book.title.chars().take(12).collect();
                    markup.push_row(vec![MarkupButton::new(
                        format!("📌追这本 {short}"),
                        format!("/追书 {}", book.record_id),
                    )]);
                }
                lines.push("\n> 点按钮选定，或回复 `/追书 <recordId>`".into());
                Ok(Some(Reply::with_markup(lines.join("\n"), markup)))
            }
        }
    }

    /// 查看追书清单（活跃项带当前可借数快照）。
    async fn list_watches(&self) -> Result<Option<Reply>> {
        let watches = self.client.load_watches();
        let active: Vec<&LibraryWatch> = watches.iter().filter(|w| w.active).collect();
        if active.is_empty() {
            return Ok(Some(Reply::text(
                "## 追书清单\n\n当前没有跟踪中的书。\n\n> `/找书 关键词` 查馆藏后点「📌追书」按钮，或回复 `/追书 书名`。"
                    .to_owned(),
            )));
        }
        let mut lines = vec![format!("## 追书清单（{} 本）\n", active.len())];
        for watch in &active {
            let state = if watch.last_loanable > 0 {
                format!("🟢 当前可借 {} 册", watch.last_loanable)
            } else {
                "🔴 暂无可借".to_owned()
            };
            lines.push(format!(
                "- 《{}》｜{}｜{}\n　取消：/追书 取消 {}",
                watch.title,
                watch.call_no.as_deref().unwrap_or("无索书号"),
                state,
                watch.record_id
            ));
        }
        Ok(Some(Reply::text(lines.join("\n"))))
    }

    /// 取消追书（墓碑停用）。
    fn cancel_watch(&self, id_or_title: &str) -> Result<Option<Reply>> {
        if id_or_title.is_empty() {
            return Ok(Some(Reply::text("用法：`/追书 取消 <recordId或书名>`".to_owned())));
        }
        match self.client.cancel_watch(id_or_title) {
            Ok(Some(title)) => Ok(Some(Reply::text(format!(
                "## 已停止跟踪\n\n《{title}》不再做馆藏到货提醒。"
            )))),
            Ok(None) => Ok(Some(Reply::text(format!(
                "## 未找到跟踪项\n\n清单里没有匹配「{id_or_title}」的书。回复 `/追书 列表` 查看。"
            )))),
            Err(err) => Ok(Some(Reply::text(format!("## 取消失败\n\n{err}")))),
        }
    }

    /// 按 recordId 添加追书：取题名、读当前馆藏做基线（只报未来 0→N）。
    async fn add_watch_by_id(&self, record_id: i64) -> Result<Option<Reply>> {
        let title = match self.client.book_title(record_id).await {
            Ok(Some(title)) => title,
            Ok(None) => {
                return Ok(Some(Reply::text(format!(
                    "## 追书失败\n\nrecordId {record_id} 没有对应书目，请从 `/找书` 结果里选。"
                ))));
            }
            Err(err) => return Ok(Some(Reply::text(format!("## 追书失败\n\n{err}")))),
        };
        let items = self.client.holdings(record_id).await.unwrap_or_default();
        let loanable = items
            .iter()
            .filter(|h| h.is_on_shelf() && !h.is_sample())
            .count() as i64;
        let call_no = items.iter().find_map(|h| h.call_no.clone());
        if let Err(err) = self.client.add_watch(LibraryWatch {
            record_id,
            title: title.clone(),
            call_no: call_no.clone(),
            active: true,
            added_at: ru41::util::now(),
            last_loanable: loanable,
            last_check: ru41::util::now(),
        }) {
            return Ok(Some(Reply::text(format!("## 追书失败\n\n{err}"))));
        }
        let hint = if loanable > 0 {
            format!("🟢 当前就有 {loanable} 册可借，直接去借！（已记录基线，不会重复提醒）")
        } else {
            "🔴 当前无可借（含样本库仅阅览），到货会第一时间通知你".to_owned()
        };
        Ok(Some(Reply::text(format!(
            "## 📌 已加入追书\n\n- 《{title}》\n- 索书号 {}\n- {hint}\n\n> 每 6 小时自动检查一次馆藏；回复 `/追书 列表` 查看，`/追书 取消 {record_id}` 停止。",
            call_no.as_deref().unwrap_or("见详情页")
        ))))
    }

    async fn renew(&self, title_query: &str) -> Result<Option<Reply>> {
        if title_query.is_empty() {
            return Ok(Some(Reply::text(
                "用法：`/续借 书名`（或从 `/借阅` 清单点按钮）".to_owned(),
            )));
        }
        if let Err(err) = self.client.ensure_session().await {
            return Ok(Some(Reply::text(format!(
                "## 图书馆登录失败\n\n{err}"
            ))));
        }
        let page = match self.client.loans().await {
            Ok(page) => page,
            Err(err) => {
                return Ok(Some(Reply::text(format!("## 续借失败\n\n{err}"))));
            }
        };
        let matches: Vec<&LoanRecord> = page
            .records
            .iter()
            .filter(|r| {
                r.title
                    .as_deref()
                    .is_some_and(|t| t.contains(title_query) || title_query.contains(t))
            })
            .collect();
        match matches.len() {
            0 => Ok(Some(Reply::text(format!(
                "## 未找到在借图书\n\n在借清单里没有匹配「{title_query}」的书。回复 `/借阅` 查看清单。"
            )))),
            1 => match self.client.renew(matches[0]).await {
                Ok(message) => {
                    let new_due = matches[0]
                        .norm_return_date
                        .clone()
                        .unwrap_or_default();
                    Ok(Some(Reply::text(format!(
                        "## ✅ 续借成功\n\n- 《{}》\n- 原应还：{new_due}\n- 平台回复：{message}\n\n> 新应还日期以图书馆为准，可回复 `/借阅` 复核。",
                        matches[0].display_title()
                    ))))
                }
                Err(err) => Ok(Some(Reply::text(format!(
                    "## 续借被拒\n\n{err}\n\n> 常见原因：该书已被续借过一次、有逾期未还、或当天已达续借次数上限。"
                )))),
            },
            _ => {
                let mut lines = vec![format!(
                    "## 匹配到 {} 本\n\n回复 `/续借 <完整书名>` 指定一本：\n",
                    matches.len()
                )];
                for record in &matches {
                    lines.push(format!(
                        "- 《{}》｜应还 {}",
                        record.display_title(),
                        record.norm_return_date.as_deref().unwrap_or("?")
                    ));
                }
                Ok(Some(Reply::text(lines.join("\n"))))
            }
        }
    }
}
