use anyhow::{Context, Result, bail};
use std::process::Command;

/// Checks the installed CLI only. Does not authenticate or invoke a model.
pub fn version() -> Result<String> {
    let output = Command::new("codex")
        .arg("--version")
        .output()
        .context("cannot start Codex CLI; install it and make sure codex is on PATH")?;
    if !output.status.success() {
        bail!("codex --version failed with {}", output.status);
    }
    Ok(String::from_utf8(output.stdout)
        .context("Codex version output was not UTF-8")?
        .trim()
        .to_owned())
}
