//! 根据启动配置装配底座；入口层无需了解协议或具体构造细节。
use crate::{adapters::agent::codex::CodexAgent, ports::agent::BackendRegistry};
use anyhow::Result;
use std::path::PathBuf;

fn codex_agent(
    bin: PathBuf,
    model: Option<String>,
    provider: Option<&crate::bootstrap::config::CodexProviderSection>,
    extra_overrides: &[String],
    output_schema: bool,
) -> Result<CodexAgent> {
    // 在切换子进程工作目录前，先解析显式的相对可执行路径。
    let executable = if bin.components().count() > 1 {
        std::fs::canonicalize(bin)?
    } else {
        bin.clone()
    };
    let mut config_overrides = match provider {
        Some(section) => crate::adapters::agent::codex::provider::overrides(section)?,
        None => Vec::new(),
    };
    config_overrides.extend(extra_overrides.iter().cloned());
    Ok(CodexAgent {
        executable,
        model,
        config_overrides,
        output_schema,
    })
}

/// CLI 覆盖项；现有 --model 和 --codex-bin 仍只影响 Codex。
#[derive(Default)]
pub struct AgentOverrides {
    pub codex_bin: Option<PathBuf>,
    pub codex_model: Option<String>,
}

/// 装配所有已配置底座；不启动进程，也不调用模型。
pub fn registry(
    agents: &super::config::AgentSection,
    overrides: AgentOverrides,
) -> Result<BackendRegistry> {
    let codex = codex_agent(
        overrides
            .codex_bin
            .or_else(|| agents.codex.bin.clone())
            .unwrap_or_else(|| "codex".into()),
        overrides.codex_model.or_else(|| agents.codex.model.clone()),
        agents.codex.provider.as_ref(),
        &agents.codex.config_overrides,
        agents.codex.output_schema.unwrap_or(true),
    )?;
    let pi = &agents.pi;
    let mut backends = BackendRegistry::default();
    backends.register(CodexAgent {
        executable: codex.executable.clone(),
        model: codex.model.clone(),
        config_overrides: codex.config_overrides.clone(),
        output_schema: codex.output_schema,
    })?;
    backends.register(crate::adapters::agent::codex::CodexSession {
        executable: codex.executable.clone(),
        model: codex.model.clone(),
        config_overrides: codex.config_overrides.clone(),
        output_schema: codex.output_schema,
    })?;
    if pi.enabled {
        anyhow::ensure!(
            pi.provider.is_none() || pi.model.is_some(),
            "agent.pi.provider requires model"
        );
        let bin = pi.bin.clone().unwrap_or_else(|| PathBuf::from("pi"));
        let executable = if bin.components().count() > 1 {
            std::fs::canonicalize(bin)?
        } else {
            bin
        };
        backends.register(crate::adapters::agent::pi::PiAgent {
            executable,
            model: pi.model.clone(),
            provider: pi.provider.clone(),
        })?;
    }
    Ok(backends)
}
