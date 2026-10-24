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
sleep 10
"#,
        ),
        model: Some("test-model".into()),
        provider: Some("test-provider".into()),
    };
    let output = tokio::time::timeout(Duration::from_secs(3), agent.run(request(dir.path())))
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
