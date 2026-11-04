use super::OpsOutcome;
use crate::{
    bootstrap::config::Config,
    ingress::cli::{Cli, VaultCommand},
    privacy::scan,
};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;

pub(crate) async fn run(cli: &Cli, command: &VaultCommand) -> OpsOutcome {
    match command {
        VaultCommand::ScanSecrets { vault, out } => {
            scan_secrets(cli, vault.as_deref(), out.as_deref())
        }
    }
}

fn scan_secrets(cli: &Cli, vault: Option<&Path>, out: Option<&Path>) -> OpsOutcome {
    let root = match vault {
        Some(path) => path.to_path_buf(),
        None => match default_vault_path(cli) {
            Ok(path) => path,
            Err(error) => return OpsOutcome::failure(error.to_string()),
        },
    };
    let report = match scan::scan_vault(&root) {
        Ok(report) => report,
        Err(error) => return OpsOutcome::failure(format!("扫描 {}: {error}", root.display())),
    };
    let value = match serde_json::to_value(&report) {
        Ok(value) => value,
        Err(error) => return OpsOutcome::failure(error.to_string()),
    };
    if let Some(out) = out
        && let Err(error) = std::fs::write(out, serde_json::to_string_pretty(&value).unwrap())
    {
        return OpsOutcome::failure(format!("写报告 {}: {error}", out.display()));
    }
    OpsOutcome::success(json!({
        "vault": root.display().to_string(),
        "report_written_to": out.map(|p| p.display().to_string()),
        "summary": {
            "scanned_files": report.scanned_files,
            "flagged_files": report.flagged_files,
            "total_hits": report.total_hits,
        },
        "files": value["files"],
    }))
}

fn default_vault_path(cli: &Cli) -> Result<std::path::PathBuf> {
    let (config, _) = Config::load(cli.config.as_deref())?;
    config
        .obsidian
        .vault_path
        .filter(|p| !p.trim().is_empty())
        .map(std::path::PathBuf::from)
        .context("未指定 --vault 且配置缺 obsidian.vault_path")
}
