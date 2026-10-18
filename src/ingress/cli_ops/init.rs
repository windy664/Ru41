use crate::{
    bootstrap::{
        config::Config,
        store::{self, StoreOptions},
    },
    ingress::cli::Cli,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

const MIMO_CATALOG_URL: &str =
    "https://example-files.cnbj1.mi-fds.com/example-files/configs/model-catalogs.json";

pub(crate) async fn run(
    cli: &Cli,
    non_interactive: bool,
    requested: Option<&str>,
) -> Result<Value> {
    let location = Config::locate(cli.config.as_deref())?;
    let provider = choose_provider(non_interactive, requested)?;
    let mut created = false;
    let mut catalog_path = None;
    if !location.exists {
        let cwd = std::env::current_dir().context("cannot resolve current directory")?;
        if provider == "mimo" {
            let path = cwd.join(".ru41/model-catalogs.json");
            download_mimo_catalog(&path).await?;
            catalog_path = Some(path);
        }
        let value = minimal_config(&provider, catalog_path.as_deref())?;
        let text = toml::to_string_pretty(&value).context("cannot render initial config")?;
        if let Some(parent) = location.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create config directory {}", parent.display()))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&location.path)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .with_context(|| format!("cannot create config {}", location.path.display()))?;
        created = true;
    }
    let (config, _) = Config::load(Some(&location.path))?;
    let options = store::resolve(
        cli.store.clone(),
        cli.db.clone(),
        cli.pg_url.clone(),
        &config,
    );
    initialize_storage(&config, &options)?;
    let active_provider = config
        .agent
        .codex
        .provider
        .as_ref()
        .and_then(|provider| provider.id.clone())
        .unwrap_or_else(|| "codex".into());
    Ok(json!({
        "created": created,
        "config_path": location.path,
        "provider": active_provider,
        "database_initialized": true,
        "model_catalog_path": catalog_path,
        "next_steps": if active_provider == "mimo" {
            vec!["export MIMO_API_KEY=<your-key>", "ru41 doctor", "ru41 status"]
        } else {
            vec!["ru41 doctor", "ru41 status"]
        },
    }))
}

fn choose_provider(non_interactive: bool, requested: Option<&str>) -> Result<String> {
    if let Some(provider) = requested {
        return Ok(provider.to_owned());
    }
    if non_interactive || !std::io::stdin().is_terminal() {
        return Ok("codex".into());
    }
    eprint!("Model provider [codex/mimo] (codex): ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    match answer.trim() {
        "" | "codex" => Ok("codex".into()),
        "mimo" => Ok("mimo".into()),
        other => bail!("unknown provider: {other}; expected codex or mimo"),
    }
}

fn minimal_config(provider: &str, catalog_path: Option<&Path>) -> Result<Value> {
    let mut codex = json!({"bin": "codex"});
    if provider == "mimo" {
        let catalog = catalog_path.context("MiMo catalog path missing")?;
        codex = json!({
            "bin": "codex",
            "model": "mimo-v2.6-flash",
            "output_schema": false,
            "config_overrides": [
                format!("model_catalog_json={:?}", catalog.to_string_lossy()),
                "model_reasoning_effort=\"none\"",
                "model_reasoning_summary=\"none\"",
                "model_context_window=1048576",
                "web_search=\"disabled\"",
            ],
            "provider": {
                "id": "mimo",
                "name": "Xiaomi MiMo",
                "base_url": "https://api.xiaomimimo.com/v1",
                "api_key_env": "MIMO_API_KEY",
                "wire_api": "responses",
            },
        });
    }
    Ok(json!({
        "store": {"kind": "sqlite", "path": ".ru41/state.db"},
        "worker": {"timeout_secs": 300, "runs_dir": ".ru41/runs", "sanitize": true},
        "agent": {"codex": codex},
    }))
}

async fn download_mimo_catalog(path: &Path) -> Result<()> {
    if path.is_file() {
        return Ok(());
    }
    let parent = path
        .parent()
        .context("MiMo catalog has no parent directory")?;
    tokio::fs::create_dir_all(parent).await?;
    let response = reqwest::get(MIMO_CATALOG_URL)
        .await
        .context("cannot download MiMo model catalog")?
        .error_for_status()
        .context("MiMo model catalog download rejected")?;
    let bytes = response.bytes().await?;
    anyhow::ensure!(
        bytes.len() <= 4 * 1024 * 1024,
        "MiMo model catalog is too large"
    );
    let value: Value = serde_json::from_slice(&bytes).context("invalid MiMo model catalog")?;
    anyhow::ensure!(
        value
            .get("models")
            .and_then(Value::as_array)
            .is_some_and(|models| !models.is_empty()),
        "MiMo model catalog has no models"
    );
    let temporary: PathBuf = path.with_extension("json.tmp");
    tokio::fs::write(&temporary, &bytes).await?;
    tokio::fs::rename(&temporary, path).await?;
    Ok(())
}

fn initialize_storage(config: &Config, options: &StoreOptions) -> Result<()> {
    let runs_dir = config
        .worker
        .runs_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(".ru41/runs"));
    std::fs::create_dir_all(&runs_dir)?;
    if let Some(parent) = runs_dir.parent() {
        std::fs::create_dir_all(parent.join("backups"))?;
    }
    if options.0 == "sqlite"
        && let Some(parent) = options.1.parent()
    {
        std::fs::create_dir_all(parent)?;
    }
    let _ = store::open_store(options)?;
    Ok(())
}
