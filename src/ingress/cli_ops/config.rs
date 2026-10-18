use super::{OpsOutcome, check};
use crate::{
    bootstrap::{
        agents::{AgentOverrides, registry},
        assembly::default_notify_route,
        config::Config,
        store::{self, StoreOptions},
    },
    ingress::cli::{Cli, ConfigCommand},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;

pub(crate) async fn run(cli: &Cli, command: &ConfigCommand) -> OpsOutcome {
    match command {
        ConfigCommand::Path => match Config::locate(cli.config.as_deref()) {
            Ok(location) => OpsOutcome::success(json!({
                "path": location.path,
                "exists": location.exists,
            })),
            Err(error) => OpsOutcome::failure(error.to_string()),
        },
        ConfigCommand::Show => show(cli),
        ConfigCommand::Check => check_config(cli),
    }
}

fn show(cli: &Cli) -> OpsOutcome {
    match redacted_config(cli.config.as_deref()) {
        Ok((path, config)) => OpsOutcome::success(json!({"path": path, "config": config})),
        Err(error) => OpsOutcome::failure(error.to_string()),
    }
}

pub(crate) fn redacted_config(
    explicit: Option<&Path>,
) -> Result<(Option<std::path::PathBuf>, Value)> {
    let (config_path, _) = match Config::load(explicit) {
        Ok((_, path)) => (path, ()),
        Err(error) => return Err(error),
    };
    let Some(path) = config_path else {
        return Ok((None, json!({})));
    };
    let text = std::fs::read_to_string(&path)?;
    let mut value: toml::Value = toml::from_str(&text)?;
    redact(&mut value);
    Ok((Some(path), serde_json::to_value(value)?))
}

fn check_config(cli: &Cli) -> OpsOutcome {
    let mut checks = Vec::new();
    let (config, config_path) = match Config::load(cli.config.as_deref()) {
        Ok(loaded) => {
            checks.push(check("config", "ok", "configuration parsed"));
            loaded
        }
        Err(error) => {
            checks.push(check("config", "error", error.to_string()));
            return OpsOutcome::report(checks);
        }
    };
    if let Some(path) = config_path {
        checks.push(check("config_path", "ok", path.display().to_string()));
    } else {
        checks.push(check("config_path", "warning", "using built-in defaults"));
    }
    let options = store::resolve(
        cli.store.clone(),
        cli.db.clone(),
        cli.pg_url.clone(),
        &config,
    );
    match validate_store(&options) {
        Ok(message) => checks.push(check("store", "ok", message)),
        Err(error) => checks.push(check("store", "error", error.to_string())),
    }
    match registry(&config.agent, AgentOverrides::default()) {
        Ok(backends) => checks.push(check(
            "agents",
            "ok",
            format!("{} backends registered", backends.descriptors().len()),
        )),
        Err(error) => checks.push(check("agents", "error", error.to_string())),
    }
    if config.notify.channel.is_some() {
        match default_notify_route(&config) {
            Ok(Some(_)) => checks.push(check("notify", "ok", "default route configured")),
            Ok(None) => checks.push(check("notify", "error", "default route is incomplete")),
            Err(error) => checks.push(check("notify", "error", error.to_string())),
        }
    } else {
        checks.push(check("notify", "warning", "no default notify route"));
    }
    OpsOutcome::report(checks)
}

pub(crate) fn validate_store(options: &StoreOptions) -> Result<String> {
    match options.0.as_str() {
        "sqlite" => Ok(format!("sqlite: {}", options.1.display())),
        "postgres" if options.2.is_some() => Ok("postgres configured".into()),
        "postgres" => anyhow::bail!("postgres URL is required"),
        other => anyhow::bail!("unknown store backend: {other}"),
    }
}

fn redact(value: &mut toml::Value) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table.iter_mut() {
                if sensitive_key(key) {
                    *value = toml::Value::String("<redacted>".into());
                } else {
                    redact(value);
                }
            }
        }
        toml::Value::Array(items) => {
            for item in items {
                redact(item);
            }
        }
        _ => {}
    }
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("secret")
        || key.contains("password")
        || key == "token"
        || key.ends_with("_token")
        || key.contains("webhook")
        || key.contains("openid")
        || key == "target"
        || key == "default_notify_target"
        || key == "app_id"
        || key == "url"
        || key == "base_url"
        || key == "feeds"
        || key == "config_overrides"
        || key == "interests"
        || key == "keywords"
        || key == "room_num"
        || key == "building_no"
        || key == "school_area_no"
        || key == "impl_type"
        || key.contains("school")
}
