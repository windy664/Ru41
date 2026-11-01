use anyhow::Result;
use ru41::ports::commands::{CommandContext, CommandHandler, Reply};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
};

use crate::service::{format_loan_reply, local_today};
use crate::source::{LibraryClient, LoanRecord};

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
        matches!(verb, "借阅" | "借书" | "图书" | "续借")
    }

    fn help(&self) -> &'static str {
        "`/借阅`：查询在借图书与应还日期（可点续借按钮）；`/续借 书名`：续借指定图书"
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
                    "用法：`/借阅` 查看在借清单；`/借阅 历史` 查看最近借阅；`/续借 书名` 续借。",
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
        if verb == "续借" {
            Box::pin(async move {
                if let Some(reply) = ctx.require_private("续借图书", "借阅记录属于个人数据")? {
                    return Ok(Some(Reply::text(reply)));
                }
                self.renew(arg.trim()).await
            })
        } else {
            self.handle(arg, ctx)
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
