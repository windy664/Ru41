use super::{OpsOutcome, config::redacted_config};
use crate::{
    adapters::database::{TaskStatusCounts, postgres::PostgresTaskStore, sqlite::SqliteTaskStore},
    bootstrap::{
        agents::{AgentOverrides, registry},
        config::Config,
        store::{self, StoreOptions},
    },
    ingress::cli::Cli,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};

pub(crate) async fn run(cli: &Cli, dump: bool) -> OpsOutcome {
    match snapshot(cli, dump) {
        Ok(value) => OpsOutcome::success(value),
        Err(error) => OpsOutcome::failure(error.to_string()),
    }
}

fn snapshot(cli: &Cli, dump: bool) -> Result<Value> {
    let (config, config_path) = Config::load(cli.config.as_deref())?;
    let options = store::resolve(
        cli.store.clone(),
        cli.db.clone(),
        cli.pg_url.clone(),
        &config,
    );
    let counts = task_counts(&options)?;
    let backends = registry(&config.agent, AgentOverrides::default())?;
    let components = json!({
        "notify": config.notify.channel,
        "qq_configured": configured(config.channel.qq.app_id.as_deref()) && configured(config.channel.qq.app_secret.as_deref()),
        "feishu_configured": configured(config.channel.feishu.webhook_url.as_deref()),
        "papers_configured": config.sources.has("arxiv"),
        "rss_configured": config.sources.has("rss"),
        "ecard_configured": config.sources.has("ecard"),
    });
    let mut value = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "config_path": config_path,
        "store": {"kind": options.0, "sqlite_path": options.1},
        "backends": backends.descriptors(),
        "tasks": counts,
        "components": components,
    });
    if dump {
        let (_, redacted) = redacted_config(cli.config.as_deref())?;
        value["environment"] = json!({
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        });
        value["config"] = redacted;
    }
    Ok(value)
}

fn task_counts(options: &StoreOptions) -> Result<TaskStatusCounts> {
    match options.0.as_str() {
        "sqlite" => SqliteTaskStore::open(&options.1)?.task_status_counts(),
        "postgres" => {
            let url = options.2.as_deref().context("postgres URL is required")?;
            tokio::task::block_in_place(|| PostgresTaskStore::open(url)?.task_status_counts())
        }
        other => anyhow::bail!("unknown store backend: {other}"),
    }
}

fn configured(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}
