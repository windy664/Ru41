//! `ru41 telegram doctor`：检查 bot token 与网络连通性，不发送任何消息。

use super::{OpsOutcome, check};
use crate::{
    bootstrap::{assembly::telegram_token, config::Config},
    channels::telegram::api::TelegramApi,
    ingress::cli::Cli,
};
use serde_json::json;

pub(crate) async fn run(cli: &Cli) -> OpsOutcome {
    let (config, _) = match Config::load(cli.config.as_deref()) {
        Ok(loaded) => loaded,
        Err(error) => return OpsOutcome::failure(error.to_string()),
    };
    let Some(token) = telegram_token(&config) else {
        return OpsOutcome::report(vec![check(
            "credentials",
            "error",
            "telegram bot token is not configured (RU41_TELEGRAM_BOT_TOKEN or channel.telegram.bot_token)",
        )]);
    };
    let api = TelegramApi::new(token);
    match api.get_me().await {
        Ok(identity) => OpsOutcome::report(vec![
            check("credentials", "ok", "telegram bot token is configured"),
            check(
                "api",
                "ok",
                format!("bot @{} (id {})", identity.username, identity.id),
            ),
        ])
        .with_field("bot_username", json!(identity.username))
        .with_field("bot_id", json!(identity.id)),
        // 错误信息里不会有 token：客户端只用脱敏地址构造错误。
        Err(error) => OpsOutcome::report(vec![
            check("credentials", "ok", "telegram bot token is configured"),
            check("api", "error", error.to_string()),
        ]),
    }
}
