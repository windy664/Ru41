//! 自定义模型提供方：把配置翻译为 codex 的 `-c` 覆盖参数。
//!
//! codex 官方机制：`model_providers.<id> = {name, base_url, env_key, wire_api}`
//! 加 `model_provider = "<id>"` 选择；`exec` 与 `app-server` 均支持 `-c` 覆盖。
//! 密钥经 `env_key` 指定的环境变量由 codex 子进程自读，宿主不接触密钥值。

/// 类型定义在 ru41-plugin-api::config（CodexSection 字段，属于插件边界）；
/// 本模块保持原路径并持有翻译逻辑。
pub use ru41_plugin_api::config::CodexProviderSection;
use anyhow::{Result, ensure};

/// 校验并生成 `-c` 覆盖参数列表。
pub fn overrides(provider: &CodexProviderSection) -> Result<Vec<String>> {
    let id = provider.id.as_deref().unwrap_or("");
    ensure!(
        !id.trim().is_empty() && id.len() <= 64,
        "provider id must contain 1..64 bytes"
    );
    let name = provider.name.clone().unwrap_or_else(|| id.to_string());
    let base_url = provider.base_url.as_deref().unwrap_or("");
    ensure!(
        base_url.starts_with("http://") || base_url.starts_with("https://"),
        "provider base_url must be http(s)"
    );
    let wire_api = provider.wire_api.as_deref().unwrap_or("chat");
    ensure!(
        matches!(wire_api, "chat" | "responses"),
        "provider wire_api must be chat or responses"
    );
    let mut table =
        format!("model_providers.{id}={{name={name:?},base_url={base_url:?},wire_api={wire_api:?}");
    if let Some(env_key) = provider
        .api_key_env
        .as_deref()
        .filter(|k| !k.trim().is_empty())
    {
        table.push_str(&format!(",env_key={env_key:?}"));
    }
    table.push('}');
    Ok(vec![table, format!("model_provider={id:?}")])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_provider_table_and_selection() {
        let section = CodexProviderSection {
            id: Some("openrouter".into()),
            name: Some("OpenRouter".into()),
            base_url: Some("https://openrouter.ai/api/v1".into()),
            api_key_env: Some("OPENROUTER_API_KEY".into()),
            wire_api: Some("chat".into()),
        };
        assert_eq!(
            overrides(&section).unwrap(),
            vec![
                "model_providers.openrouter={name=\"OpenRouter\",base_url=\"https://openrouter.ai/api/v1\",wire_api=\"chat\",env_key=\"OPENROUTER_API_KEY\"}".to_string(),
                "model_provider=\"openrouter\"".to_string(),
            ]
        );
    }

    #[test]
    fn defaults_are_sensible() {
        let section = CodexProviderSection {
            id: Some("local".into()),
            name: None,
            base_url: Some("http://127.0.0.1:8080/v1".into()),
            api_key_env: None,
            wire_api: None,
        };
        let args = overrides(&section).unwrap();
        assert_eq!(
            args[0],
            "model_providers.local={name=\"local\",base_url=\"http://127.0.0.1:8080/v1\",wire_api=\"chat\"}"
        );
    }

    #[test]
    fn rejects_bad_input() {
        let bad_url = CodexProviderSection {
            base_url: Some("ftp://x".into()),
            ..Default::default()
        };
        assert!(overrides(&bad_url).is_err());
        let bad_wire = CodexProviderSection {
            id: Some("x".into()),
            base_url: Some("https://a.b".into()),
            wire_api: Some("grpc".into()),
            ..Default::default()
        };
        assert!(overrides(&bad_wire).is_err());
    }
}
