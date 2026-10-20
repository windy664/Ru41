use crate::ingress::cli::{Cli, Command, QqCommand};
use anyhow::Result;
use serde_json::{Value, json};

mod config;
mod doctor;
mod init;
mod qq;
mod status;

pub struct OpsOutcome {
    pub value: Value,
    pub success: bool,
}

impl OpsOutcome {
    pub(crate) fn success(value: Value) -> Self {
        Self {
            value,
            success: true,
        }
    }

    pub(crate) fn failure(message: impl Into<String>) -> Self {
        Self {
            value: json!({"ok": false, "error": message.into()}),
            success: false,
        }
    }

    pub(crate) fn report(checks: Vec<Value>) -> Self {
        let success = checks
            .iter()
            .all(|check| check.get("status").and_then(Value::as_str) != Some("error"));
        Self {
            value: json!({"ok": success, "checks": checks}),
            success,
        }
    }

    pub(crate) fn with_field(mut self, name: &str, value: Value) -> Self {
        self.value[name] = value;
        self
    }
}

pub(crate) fn check(name: &str, status: &str, message: impl Into<String>) -> Value {
    json!({"name": name, "status": status, "message": message.into()})
}

pub async fn try_run(cli: &Cli) -> Result<Option<OpsOutcome>> {
    match &cli.command {
        Command::Init {
            non_interactive,
            provider,
        } => Ok(Some(OpsOutcome::success(
            init::run(cli, *non_interactive, provider.as_deref()).await?,
        ))),
        Command::Doctor { deep } => Ok(Some(doctor::run(cli, *deep).await)),
        Command::Config { command } => Ok(Some(config::run(cli, command).await)),
        Command::Qq {
            command: QqCommand::Doctor { credentials },
        } => Ok(Some(qq::run(cli, credentials).await)),
        Command::Qq {
            command: QqCommand::Onboard,
        } => Ok(Some(qq::onboard(cli).await)),
        Command::Status => Ok(Some(status::run(cli, false).await)),
        Command::Dump => Ok(Some(status::run(cli, true).await)),
        _ => Ok(None),
    }
}
