use super::{OpsOutcome, check, config::validate_store};
use crate::{
    adapters::agent::codex,
    bootstrap::{
        agents::{AgentOverrides, registry},
        config::Config,
        store::{self, StoreOptions},
    },
    domain::RunContract,
    ingress::cli::Cli,
    ports::agent::{AgentRequest, BackendRegistry},
};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{path::Path, process::Command, time::Duration};

pub(crate) async fn run(cli: &Cli, deep: bool) -> OpsOutcome {
    let mut checks = Vec::new();
    let (config, config_path) = match Config::load(cli.config.as_deref()) {
        Ok(loaded) => {
            checks.push(check("config", "ok", "configuration parsed"));
            loaded
        }
        Err(error) => {
            checks.push(check("config", "error", error.to_string()));
            return OpsOutcome::report(checks)
                .with_field("deep", json!(deep))
                .with_field("model_invoked", json!(false));
        }
    };
    if let Some(path) = config_path.as_deref() {
        checks.push(config_permissions(path));
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
    if options.0 == "sqlite" && !options.1.exists() {
        checks.push(check(
            "database_file",
            "warning",
            "database does not exist yet; run ru41 init",
        ));
    }
    let runs_dir = config
        .worker
        .runs_dir
        .clone()
        .unwrap_or_else(|| ".ru41/runs".into());
    checks.push(if runs_dir.is_dir() {
        check("runs_dir", "ok", runs_dir.display().to_string())
    } else {
        check(
            "runs_dir",
            "warning",
            format!("{} does not exist yet", runs_dir.display()),
        )
    });
    let backends = match registry(&config.agent, AgentOverrides::default()) {
        Ok(backends) => {
            checks.push(check(
                "agents",
                "ok",
                format!("{} backends registered", backends.descriptors().len()),
            ));
            Some(backends)
        }
        Err(error) => {
            checks.push(check("agents", "error", error.to_string()));
            None
        }
    };
    let codex_bin = config
        .agent
        .codex
        .bin
        .clone()
        .unwrap_or_else(|| "codex".into());
    match codex::version_with(&codex_bin) {
        Ok(version) => checks.push(check("codex", "ok", version)),
        Err(error) => checks.push(check("codex", "error", error.to_string())),
    }
    if config.agent.pi.enabled {
        let pi_bin = config.agent.pi.bin.clone().unwrap_or_else(|| "pi".into());
        match executable_version(&pi_bin) {
            Ok(version) => checks.push(check("pi", "ok", version)),
            Err(error) => checks.push(check("pi", "error", error.to_string())),
        }
    }
    let mut model_invoked = false;
    if deep {
        match check_storage_deep(&options) {
            Ok(message) => checks.push(check("storage_deep", "ok", message)),
            Err(error) => checks.push(check("storage_deep", "error", error.to_string())),
        }
        if let Some(backends) = backends.as_ref() {
            model_invoked = true;
            match model_smoke(backends).await {
                Ok(()) => checks.push(check(
                    "model_smoke",
                    "ok",
                    "backend returned a valid result",
                )),
                Err(error) => checks.push(check("model_smoke", "error", error.to_string())),
            }
        }
    }
    OpsOutcome::report(checks)
        .with_field("deep", json!(deep))
        .with_field("model_invoked", json!(model_invoked))
}

fn config_permissions(path: &Path) -> serde_json::Value {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.permissions().mode() & 0o077 != 0 => check(
                "config_permissions",
                "warning",
                "config is accessible by group or other users",
            ),
            Ok(_) => check("config_permissions", "ok", "config permissions are private"),
            Err(error) => check("config_permissions", "error", error.to_string()),
        }
    }
    #[cfg(not(unix))]
    {
        check("config_path", "ok", path.display().to_string())
    }
}

fn executable_version(executable: &Path) -> Result<String> {
    let output = Command::new(executable).arg("--version").output()?;
    anyhow::ensure!(
        output.status.success(),
        "{} --version failed",
        executable.display()
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn check_storage_deep(options: &StoreOptions) -> Result<String> {
    match options.0.as_str() {
        "sqlite" => {
            anyhow::ensure!(options.1.is_file(), "SQLite database does not exist");
            let connection = rusqlite::Connection::open_with_flags(
                &options.1,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let result: String =
                connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            anyhow::ensure!(result == "ok", "SQLite integrity_check returned {result}");
            Ok("SQLite integrity_check ok".into())
        }
        "postgres" => {
            let url = options.2.as_deref().context("postgres URL is required")?;
            tokio::task::block_in_place(|| {
                let mut client = postgres::Client::connect(url, postgres::NoTls)?;
                client.query_one("SELECT 1", &[])?;
                Ok("postgres connection ok".into())
            })
        }
        other => bail!("unknown store backend: {other}"),
    }
}

async fn model_smoke(backends: &BackendRegistry) -> Result<()> {
    let backend = backends.get("codex")?;
    let run_dir = std::env::temp_dir().join(format!("ru41-doctor-{}", rand::random::<u64>()));
    tokio::fs::create_dir_all(&run_dir).await?;
    let request = AgentRequest {
        context: None,
        prompt: "Return only a JSON object with summary set to ok.".into(),
        run_dir: run_dir.clone(),
        contract: RunContract::AnalysisV1,
    };
    let result = tokio::time::timeout(Duration::from_secs(120), backend.run(request)).await;
    let _ = tokio::fs::remove_dir_all(&run_dir).await;
    match result {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(_) => bail!("model smoke timed out"),
    }
}
