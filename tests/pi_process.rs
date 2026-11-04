#![cfg(unix)]
use ru41::{
    adapters::agent::pi::PiAgent,
    domain::RunContract,
    ports::agent::{AgentBackend, AgentRequest},
};
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};

fn executable(dir: &Path, body: &str) -> std::path::PathBuf {
    let file = dir.join("fake-pi");
    std::fs::write(&file, format!("#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > args.txt\nread -r prompt\nprintf '%s\\n' \"$prompt\" > prompt.json\n{body}\n")).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
    file
}
fn request(dir: &Path) -> AgentRequest {
    AgentRequest {
        context: None,
        run_dir: dir.into(),
        prompt: "测试 Unicode".into(),
        contract: RunContract::AnalysisV1,
    }
}
#[tokio::test]
async fn waits_for_settled_and_uses_final_message() {
    let dir = tempfile::tempdir().unwrap();
    let agent = PiAgent {
        executable: executable(
            dir.path(),
            r#"
printf '%s\n' '{"id":"ru41-prompt","type":"response","success":true,"data":{"disposition":"started"}}'
printf '%s\n' '{"type":"message_end","message":{"role":"assistant","stopReason":"error","content":[]}}'
printf '%s\n' '{"type":"agent_end"}'
printf '%s\n' '{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"{\"summary\":\"完成\"}"}]}}'
printf '%s\n' '{"type":"agent_settled","aborted":false}'
sleep 30
"#,
        ),
        model: Some("test-model".into()),
        provider: Some("test-provider".into()),
        ..Default::default()
    };
    // 断言 agent 在 agent_settled 后即返回、不等进程退出：超时上限需远小于
    // fake 脚本的 sleep 时长才有效，但又得给 CI 并行负载留足拉起余量。
    let output = tokio::time::timeout(Duration::from_secs(10), agent.run(request(dir.path())))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.result.summary, "完成");
    assert!(output.session_id.is_none());
    let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
    for flag in [
        "--no-tools",
        "--no-extensions",
        "--no-context-files",
        "--no-mcp",
        "--no-session",
        "--model",
        "test-model",
        "test-provider",
    ] {
        assert!(args.lines().any(|a| a == flag));
    }
    let prompt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("prompt.json")).unwrap())
            .unwrap();
    assert_eq!(prompt["message"], "测试 Unicode");
}
#[tokio::test]
async fn rejects_failed_aborted_missing_and_malformed_results() {
    for body in [
        r#"printf '%s\n' '{"id":"ru41-prompt","type":"response","success":false}'"#,
        r#"printf '%s\n' '{"type":"agent_settled","aborted":true}'"#,
        r#"printf '%s\n' '{"id":"ru41-prompt","type":"response","success":true}' '{"type":"agent_end"}'"#,
        r#"printf '%s\n' '{"id":"ru41-prompt","type":"response","success":true}' '{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"not json"}]}}' '{"type":"agent_settled","aborted":false}'"#,
        r#"printf '%s\n' '{"type":"extension_ui_request"}'"#,
        r#"printf '%s\n' 'not json'"#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let agent = PiAgent {
            executable: executable(dir.path(), body),
            ..Default::default()
        };
        assert!(
            tokio::time::timeout(Duration::from_secs(3), agent.run(request(dir.path())))
                .await
                .unwrap()
                .is_err()
        );
    }
}
#[tokio::test]
async fn bounds_output_and_cancels_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let agent = PiAgent {
        executable: executable(dir.path(), "head -c 1048577 /dev/zero"),
        ..Default::default()
    };
    assert!(agent.run(request(dir.path())).await.is_err());
    let agent = PiAgent {
        executable: executable(dir.path(), "echo $$ > pid.txt\nsleep 30"),
        ..Default::default()
    };
    let result =
        tokio::time::timeout(Duration::from_millis(150), agent.run(request(dir.path()))).await;
    assert!(result.is_err());
    let pid: i32 = std::fs::read_to_string(dir.path().join("pid.txt"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    for _ in 0..50 {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("Pi process survived cancellation");
}

#[tokio::test]
async fn worker_routes_to_pi_alongside_codex() {
    use ru41::{
        adapters::{agent::codex::CodexAgent, database::sqlite::SqliteTaskStore},
        domain::TaskStatus,
        ports::{agent::BackendRegistry, storage::TaskStore},
        privacy::NoopSanitizer,
        runtime::worker::{Worker, WorkerConfig},
    };
    use std::sync::Arc;
    use tokio::sync::watch;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("db")).unwrap());
    let agent = PiAgent {
        executable: executable(
            dir.path(),
            r#"
printf '%s\n' '{"id":"ru41-prompt","type":"response","success":true}' '{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"{\"summary\":\"pi result\"}"}]}}' '{"type":"agent_settled","aborted":false}'
"#,
        ),
        ..Default::default()
    };
    let mut registry = BackendRegistry::default();
    registry.register(CodexAgent::default()).unwrap();
    registry.register(agent).unwrap();
    let task = store
        .enqueue_for("pi:worker", "hello", "pi", RunContract::AnalysisV1)
        .unwrap()
        .task;
    let mut worker = Worker {
        store: store.clone(),
        backends: Arc::new(registry),
        sanitizer: Arc::new(NoopSanitizer),
        knowledge: None,
        config: WorkerConfig {
            worker_id: "test".into(),
            runs_dir: dir.path().join("runs"),
            timeout: Duration::from_secs(3),
            lease_secs: 30,
            retry_delay_secs: 10,
        },
    };
    let (_sender, receiver) = watch::channel(false);
    assert_eq!(
        worker.run_once(receiver).await.unwrap().unwrap().status,
        TaskStatus::Succeeded
    );
    let attempts = store.attempts(task.id).unwrap();
    assert_eq!(attempts[0].backend_id, "pi");
    assert_eq!(
        store.get(task.id).unwrap().unwrap().result.unwrap().summary,
        "pi result"
    );
}

#[test]
fn pi_configuration_is_opt_in() {
    use ru41::bootstrap::config::Config;
    let config: Config = toml::from_str("").unwrap();
    assert!(!config.agent.pi.enabled);
    let config: Config =
        toml::from_str("[agent.pi]\nenabled=true\nbin='pi'\nmodel='test/model'\n").unwrap();
    assert!(config.agent.pi.enabled);
    assert_eq!(config.agent.pi.model.as_deref(), Some("test/model"));
}

#[test]
fn pi_provider_configuration_is_parsed() {
    use ru41::bootstrap::config::Config;
    let config: Config = toml::from_str(
        "[agent.pi]\nenabled=true\nmodel='minimax/MiniMax-M3'\nprovider='minimax'\n\
         base_url='https://api.minimaxi.chat/anthropic'\napi='anthropic-messages'\n\
         api_key_env='MINIMAX_API_KEY'\nsession=true\n",
    )
    .unwrap();
    let pi = config.agent.pi;
    assert_eq!(
        pi.base_url.as_deref(),
        Some("https://api.minimaxi.chat/anthropic")
    );
    assert_eq!(pi.api.as_deref(), Some("anthropic-messages"));
    assert_eq!(pi.api_key_env.as_deref(), Some("MINIMAX_API_KEY"));
    assert_eq!(pi.session, Some(true));
}

#[tokio::test]
async fn reports_token_usage_from_message_update() {
    let dir = tempfile::tempdir().unwrap();
    let agent = PiAgent {
        executable: executable(
            dir.path(),
            r#"
printf '%s\n' '{"id":"ru41-prompt","type":"response","success":true,"data":{"disposition":"started"}}'
printf '%s\n' '{"type":"message_update","usage":{"input":120,"output":30,"cacheRead":40,"cacheWrite":0,"totalTokens":190},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"x"}}'
printf '%s\n' '{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"{\"summary\":\"ok\"}"}]}}'
printf '%s\n' '{"type":"agent_settled","aborted":false}'
"#,
        ),
        ..Default::default()
    };
    let output = tokio::time::timeout(Duration::from_secs(10), agent.run(request(dir.path())))
        .await
        .unwrap()
        .unwrap();
    let usage = output.usage.expect("usage should be reported");
    assert_eq!(usage.input_tokens, 120);
    assert_eq!(usage.cached_input_tokens, 40);
    assert_eq!(usage.output_tokens, 30);
    assert_eq!(usage.total_tokens, 190);
}

#[tokio::test]
async fn custom_endpoint_writes_models_json_without_key_value() {
    let dir = tempfile::tempdir().unwrap();
    let agent = PiAgent {
        executable: executable(
            dir.path(),
            r#"
printf '%s\n' '{"id":"ru41-prompt","type":"response","success":true,"data":{"disposition":"started"}}'
printf '%s\n' '{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"{\"summary\":\"ok\"}"}]}}'
printf '%s\n' '{"type":"agent_settled","aborted":false}'
"#,
        ),
        model: Some("minimax/MiniMax-M3".into()),
        provider: Some("minimax".into()),
        base_url: Some("https://api.minimaxi.chat/anthropic".into()),
        api: Some("anthropic-messages".into()),
        api_key_env: Some("RU41_TEST_PI_KEY".into()),
        ..Default::default()
    };
    // models.json 内容与写盘无关，直接断言结构：密钥只以环境变量引用出现。
    let models = agent.models_json().unwrap();
    let provider = &models["providers"]["minimax"];
    assert_eq!(provider["baseUrl"], "https://api.minimaxi.chat/anthropic");
    assert_eq!(provider["api"], "anthropic-messages");
    assert_eq!(provider["apiKey"], "$RU41_TEST_PI_KEY");
    assert_eq!(provider["models"][0]["id"], "MiniMax-M3");
    let raw = serde_json::to_string(&models).unwrap();
    assert!(!raw.contains("sk-"));
    // 端点协议不支持时直接报错。
    let bad = PiAgent {
        api: Some("grpc".into()),
        ..Default::default()
    };
    assert!(bad.models_json().is_err());
}

#[tokio::test]
async fn missing_key_env_is_reported_as_config_error() {
    let dir = tempfile::tempdir().unwrap();
    let agent = PiAgent {
        executable: executable(dir.path(), "sleep 30"),
        api_key_env: Some("RU41_TEST_PI_ABSENT_KEY".into()),
        ..Default::default()
    };
    let error = match tokio::time::timeout(Duration::from_secs(5), agent.run(request(dir.path())))
        .await
        .unwrap()
    {
        Ok(_) => panic!("missing key env should fail"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("RU41_TEST_PI_ABSENT_KEY"),
        "{error:#}"
    );
}

#[tokio::test]
async fn session_mode_reports_session_id_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let agent = PiAgent {
        executable: executable(
            dir.path(),
            r#"
printf '%s\n' '{"id":"ru41-prompt","type":"response","success":true,"data":{"disposition":"started"}}'
printf '%s\n' '{"id":"ru41-state","type":"response","success":true,"data":{"sessionId":"sess-42"}}'
printf '%s\n' '{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"{\"summary\":\"ok\"}"}]}}'
printf '%s\n' '{"type":"agent_settled","aborted":false}'
"#,
        ),
        session: true,
        id: "pi-session".into(),
        ..Default::default()
    };
    assert!(agent.descriptor().supports_resume);
    let output = tokio::time::timeout(Duration::from_secs(10), agent.run(request(dir.path())))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.session_id.as_deref(), Some("sess-42"));
    let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
    // 会话模式不再传 --no-session。
    assert!(!args.lines().any(|a| a == "--no-session"));
}
