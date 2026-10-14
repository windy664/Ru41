use ru41::bootstrap::{
    agents::{AgentOverrides, registry},
    config::Config,
};

#[test]
fn cli_model_override_does_not_leak_into_pi() {
    let config: Config = toml::from_str(
        r#"
[agent.codex]
model = "codex-default"
[agent.pi]
enabled = true
model = "pi-model"
provider = "pi-provider"
"#,
    )
    .unwrap();
    let backends = registry(
        &config.agent,
        AgentOverrides {
            codex_model: Some("codex-override".into()),
            ..Default::default()
        },
    )
    .unwrap();
    for id in ["codex", "codex-session"] {
        assert_eq!(
            backends.get(id).unwrap().descriptor().model.as_deref(),
            Some("codex-override")
        );
    }
    assert_eq!(
        backends.get("pi").unwrap().descriptor().model.as_deref(),
        Some("pi-model")
    );
}

#[test]
fn pi_is_opt_in_and_provider_requires_model() {
    let config = Config::default();
    assert!(
        registry(&config.agent, AgentOverrides::default())
            .unwrap()
            .get("pi")
            .is_err()
    );
    let config: Config =
        toml::from_str("[agent.pi]\nenabled = true\nprovider = 'example'").unwrap();
    assert!(registry(&config.agent, AgentOverrides::default()).is_err());
}
