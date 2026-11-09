use super::OpsOutcome;
use crate::{
    bootstrap::{config::Config, store},
    ingress::cli::{Cli, VaultCommand},
    privacy::{migrate, scan},
};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;

pub(crate) async fn run(cli: &Cli, command: &VaultCommand) -> OpsOutcome {
    match command {
        VaultCommand::ScanSecrets { vault, out } => {
            scan_secrets(cli, vault.as_deref(), out.as_deref())
        }
        VaultCommand::MigrateSecrets {
            vault,
            apply,
            exclude,
        } => migrate_secrets(cli, vault.as_deref(), *apply, exclude),
    }
}

/// 明文密码迁移：计划模式只报位置与名字；--apply 才写凭据库并改写笔记。
fn migrate_secrets(cli: &Cli, vault: Option<&Path>, apply: bool, exclude: &[String]) -> OpsOutcome {
    let root = match vault {
        Some(path) => path.to_path_buf(),
        None => match default_vault_path(cli) {
            Ok(path) => path,
            Err(error) => return OpsOutcome::failure(error.to_string()),
        },
    };
    if !apply {
        let plan = match migrate::plan(&root, exclude) {
            Ok(plan) => plan,
            Err(error) => return OpsOutcome::failure(format!("扫描 {}: {error}", root.display())),
        };
        return OpsOutcome::success(json!({
            "mode": "dry-run",
            "vault": root.display().to_string(),
            "planned": plan.entries.len(),
            "entries": plan.entries,
            "hint": "加 --apply 执行：写凭据库并把笔记里的明文换成凭据引用",
        }));
    }
    let (config, _) = match Config::load(cli.config.as_deref()) {
        Ok(pair) => pair,
        Err(error) => return OpsOutcome::failure(error.to_string()),
    };
    let store_options = store::resolve(
        cli.store.clone(),
        cli.db.clone(),
        cli.pg_url.clone(),
        &config,
    );
    let store = match store::open_store(&store_options) {
        Ok(store) => store,
        Err(error) => return OpsOutcome::failure(error.to_string()),
    };
    let applied = match migrate::apply(&root, store.as_ref(), crate::util::now(), exclude) {
        Ok(applied) => applied,
        Err(error) => return OpsOutcome::failure(error.to_string()),
    };
    OpsOutcome::success(json!({
        "mode": "applied",
        "vault": root.display().to_string(),
        "credentials": applied.credentials,
        "rewritten_files": applied.rewritten_files,
        "entries": applied.entries,
    }))
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
