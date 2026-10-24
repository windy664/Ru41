//! 微助教 QQ 命令：把微信里复制的服务号链接**直接粘贴进单聊**即完成换绑
//! （openid 落盘为文件，轮询客户端每请求热加载，无需重启容器）；
//! `/微助教` 查看绑定与存活状态。写入凭证要求单聊 + 已绑定身份。

use crate::{
    modules::teachermate::{config::extract_openid, source::TeachermateClient},
    ports::commands::{CommandContext, CommandHandler, Reply},
};
use anyhow::Result;
use std::{future::Future, path::PathBuf, pin::Pin, time::UNIX_EPOCH};

/// 微助教换绑/状态命令。openid 文件与轮询容器共享（windy2 上位于 /data 挂载内）。
pub struct TeachermateQqCommand {
    openid_path: PathBuf,
}

impl TeachermateQqCommand {
    pub fn new(openid_path: impl Into<PathBuf>) -> Self {
        Self {
            openid_path: openid_path.into(),
        }
    }

    fn stored_openid(&self) -> Option<String> {
        std::fs::read_to_string(&self.openid_path)
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|v| !v.is_empty())
    }

    fn file_stamp(&self) -> Option<String> {
        let secs = std::fs::metadata(&self.openid_path)
            .and_then(|meta| meta.modified())
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs();
        chrono::DateTime::from_timestamp(secs as i64, 0)
            .map(|dt| dt.format("%m-%d %H:%M UTC").to_string())
    }
}

impl CommandHandler for TeachermateQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(verb, "微助教" | "teachermate")
    }

    fn help(&self) -> &'static str {
        "`/微助教`：查看 openid 绑定状态；微信复制微助教链接后直接粘贴到对话即可换绑"
    }

    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if !arg.is_empty() {
                return Ok(None);
            }
            if let Some(reply) = ctx.require_private("查询微助教状态", "openid 属于个人凭证")?
            {
                return Ok(Some(Reply::text(reply)));
            }
            let Some(openid) = self.stored_openid() else {
                return Ok(Some(Reply::text(
                    "## 未绑定\n\n微信里打开微助教服务号任意页面，右上角 ⋯ → 复制链接，\
                     然后把链接直接粘贴到这里。"
                        .to_owned(),
                )));
            };
            let masked = format!("…{}", &openid[openid.len().saturating_sub(4)..]);
            let stamp = self.file_stamp().unwrap_or_else(|| "未知".to_owned());
            let client = TeachermateClient::new(openid);
            let health = match client.role().await {
                Ok(roles) => roles
                    .first()
                    .and_then(|r| r.college_name.clone())
                    .map(|college| format!("✅ 有效（{college}）"))
                    .unwrap_or_else(|| "✅ 有效".to_owned()),
                Err(err) => format!("❌ 已失效——粘贴新链接即可换绑（{err}）"),
            };
            Ok(Some(Reply::text(format!(
                "## 微助教\n\n- openid：{masked}\n- 写入时间：{stamp}\n- 状态：{health}\n- 轮询：签到 60s · 作业 30min"
            ))))
        })
    }

    fn handle_message<'a>(
        &'a self,
        content: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            // 只拦截提到微助教域名的消息，其余交回 Agent 通道
            if !content.contains("teachermate.cn") {
                return Ok(None);
            }
            if let Some(reply) = ctx.require_private("换绑微助教 openid", "openid 属于个人凭证")?
            {
                return Ok(Some(Reply::text(reply)));
            }
            let Some(openid) = extract_openid(content) else {
                return Ok(Some(Reply::text(
                    "链接里没有 openid= 参数。请在微信里重新打开微助教页面，\
                     用右上角 ⋯ → 复制链接 拿完整地址再发一次。"
                        .to_owned(),
                )));
            };
            // 先验证后落盘：坏凭证不写入，避免“已绑定但失效”的假状态
            let client = TeachermateClient::new(openid.clone());
            let identity = match client.role().await {
                Ok(roles) => roles
                    .first()
                    .map(|r| {
                        format!(
                            "{} {} · {} · {}",
                            r.college_name.clone().unwrap_or_default(),
                            r.department_name.clone().unwrap_or_default(),
                            r.class_name.clone().unwrap_or_default(),
                            r.student_number.clone().unwrap_or_default(),
                        )
                    })
                    .unwrap_or_else(|| "验证通过".to_owned()),
                Err(err) => format!("验证失败：{err}"),
            };
            if identity.starts_with("验证失败") {
                return Ok(Some(Reply::text(format!(
                    "## ❌ 换绑失败\n\n{identity}\n\n链接可能已被微信刷新作废：\
                     请在微信里重新打开微助教页面，重新 ⋯ → 复制链接 再发一次。"
                ))));
            }
            if let Some(parent) = self.openid_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&self.openid_path, openid.as_bytes())
                .map_err(|e| anyhow::anyhow!("写 openid 文件失败: {e}"))?;
            Ok(Some(Reply::text(format!(
                "## ✅ 微助教已换绑\n\n- 身份：{identity}\n- 生效：下一次轮询（≤60s）自动启用，无需重启\n\n\
                 提醒：平时别在微信里点微助教链接，点了就会再次失效。"
            ))))
        })
    }
}
