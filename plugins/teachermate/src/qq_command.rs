//! 微助教换绑：把微信里复制的服务号链接**直接粘贴进单聊**即完成换绑
//! （openid 落盘为文件，轮询客户端每请求热加载，无需重启容器）。
//! 写入凭证要求单聊 + 已绑定身份；不占额外 QQ 命令入口。

use crate::{config::extract_openid, source::TeachermateClient};
use anyhow::Result;
use ru41::ports::commands::{CommandContext, Reply, SimpleCommand};
use std::path::PathBuf;

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
}

impl SimpleCommand for TeachermateQqCommand {
    fn verbs(&self) -> &'static [&'static str] {
        &[]
    }

    fn help(&self) -> &'static str {
        "（自动）微信复制微助教服务号链接后直接粘贴到对话即可换绑"
    }

    fn help_section(&self) -> &'static str {
        "课程与学习"
    }

    async fn run(&self, _arg: &str, _ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        Ok(None)
    }

    async fn run_message(&self, content: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
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
        let identity = match ru41::plugins::bridge(async move { client.role().await }).await {
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
    }
}
