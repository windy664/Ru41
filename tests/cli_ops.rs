#![cfg(unix)]

use serde_json::Value;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn run(root: &Path, args: &[&str]) -> Output {
    run_with_ru41_home(root, args, None)
}

fn run_with_ru41_home(root: &Path, args: &[&str], ru41_home: Option<&Path>) -> Output {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ru41"));
    command
        .current_dir(root)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HOME", home);
    if let Some(ru41_home) = ru41_home {
        command.env("RU41_HOME", ru41_home);
    }
    command.output().unwrap()
}

fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn config_arg(root: &Path) -> String {
    root.join("ru41.toml").display().to_string()
}

#[test]
fn init_is_idempotent_and_operation_commands_share_state() {
    let root = tempfile::tempdir().unwrap();
    let config = config_arg(root.path());
    let first = json(&run(
        root.path(),
        &["--config", &config, "init", "--non-interactive"],
    ));
    assert_eq!(first["created"], true);
    assert!(root.path().join(".ru41/state.db").is_file());
    assert!(root.path().join(".ru41/runs").is_dir());
    assert!(root.path().join(".ru41/backups").is_dir());
    let initial = std::fs::read_to_string(root.path().join("ru41.toml")).unwrap();
    let mode = std::fs::metadata(root.path().join("ru41.toml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0);

    let second = json(&run(
        root.path(),
        &["--config", &config, "init", "--non-interactive"],
    ));
    assert_eq!(second["created"], false);
    assert_eq!(
        std::fs::read_to_string(root.path().join("ru41.toml")).unwrap(),
        initial
    );

    let path = json(&run(root.path(), &["--config", &config, "config", "path"]));
    assert_eq!(path["exists"], true);
    let show = json(&run(root.path(), &["--config", &config, "config", "show"]));
    assert_eq!(show["config"]["store"]["kind"], "sqlite");
    let check = json(&run(root.path(), &["--config", &config, "config", "check"]));
    assert_eq!(check["ok"], true);
    let status = json(&run(root.path(), &["--config", &config, "status"]));
    assert_eq!(status["tasks"]["queued"], 0);
    assert_eq!(status["backends"].as_array().unwrap().len(), 2);
    let dump = json(&run(root.path(), &["--config", &config, "dump"]));
    assert_eq!(dump["environment"]["os"], std::env::consts::OS);
}

#[test]
fn init_uses_ru41_home_layout() {
    let root = tempfile::tempdir().unwrap();
    let ru41_home = tempfile::tempdir().unwrap();
    let output = json(&run_with_ru41_home(
        root.path(),
        &["init", "--non-interactive"],
        Some(ru41_home.path()),
    ));
    let config_path = ru41_home.path().join("ru41.toml");
    assert_eq!(output["created"], true);
    assert_eq!(output["config_path"], config_path.display().to_string());
    assert!(config_path.is_file());
    let text = std::fs::read_to_string(config_path).unwrap();
    let data_dir = ru41_home.path().join("data");
    assert!(text.contains(&data_dir.join("state.db").display().to_string()));
    assert!(text.contains(&data_dir.join("runs").display().to_string()));
    assert!(ru41_home.path().join("data/state.db").is_file());
    assert!(ru41_home.path().join("data/runs").is_dir());
}

#[test]
fn missing_and_invalid_configs_are_reported_without_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let config = config_arg(root.path());
    let path = json(&run(root.path(), &["--config", &config, "config", "path"]));
    assert_eq!(path["exists"], false);
    assert!(!root.path().join("ru41.toml").exists());

    std::fs::write(root.path().join("ru41.toml"), "not toml [").unwrap();
    let output = run(root.path(), &["--config", &config, "config", "check"]);
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], false);
}

#[test]
fn sensitive_config_is_redacted_from_show_and_dump() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("state.db");
    std::fs::write(
        root.path().join("ru41.toml"),
        format!(
            r#"
[store]
kind = "sqlite"
path = "{}"
[agent.codex]
bin = "codex"
config_overrides = ['api_key="override-secret"']
[agent.codex.provider]
id = "mimo"
base_url = "https://user:pass@example.com/v1"
api_key_env = "MIMO_API_KEY"
wire_api = "responses"
[channel.qq]
app_id = "private-app-id"
app_secret = "private-app-secret"
default_notify_target = "user:private-openid"
[channel.feishu]
webhook_url = "https://example.com/private-webhook-token"
[sources.rss]
feeds = ["https://private.example/feed"]
interests = "private interests"
keywords = ["private keyword"]
[sources.ecard]
base_url = "https://private-school.example"
token = "private-ecard-token"
impl_type = "private-impl"
school_area_no = "private-area"
building_no = "private-building"
room_num = "private-room"
[api]
token = "private-api-token"
"#,
            db.display()
        ),
    )
    .unwrap();
    let config = config_arg(root.path());
    let show = run(root.path(), &["--config", &config, "config", "show"]);
    let dump = run(root.path(), &["--config", &config, "dump"]);
    for output in [show, dump] {
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        for secret in [
            "override-secret",
            "user:pass",
            "private-app-id",
            "private-app-secret",
            "private-openid",
            "private-webhook-token",
            "private.example",
            "private interests",
            "private keyword",
            "private-school",
            "private-ecard-token",
            "private-impl",
            "private-area",
            "private-building",
            "private-room",
            "private-api-token",
        ] {
            assert!(!text.contains(secret), "leaked {secret} in {text}");
        }
    }
}

#[test]
fn plugin_config_is_shown_effectively_but_excluded_from_worker_redaction() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("ru41.toml"),
        "[sources.rss]\ninterval_secs = 300\n",
    )
    .unwrap();
    let plugin_dir = root.path().join("plugins.d/rss");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("config.toml"),
        "[rss]\ninterval_secs = 61\ntoken = \"plugin-secret\"\n",
    )
    .unwrap();

    let config = config_arg(root.path());
    let show = json(&run(root.path(), &["--config", &config, "config", "show"]));
    assert_eq!(show["config"]["sources"]["rss"]["interval_secs"], 61);
    assert_eq!(show["config"]["sources"]["rss"]["token"], "<redacted>");
    let text = serde_json::to_string(&show).unwrap();
    assert!(!text.contains("plugin-secret"));

    let worker = root.path().join("worker.toml");
    let out = worker.display().to_string();
    let redacted = json(&run(
        root.path(),
        &["--config", &config, "config", "redact", "--out", &out],
    ));
    assert_eq!(redacted["removed_sources"], true);
    let worker_text = std::fs::read_to_string(worker).unwrap();
    assert!(!worker_text.contains("sources"));
    assert!(!worker_text.contains("plugin-secret"));
    assert!(!worker_text.contains("interval_secs = 61"));
}

#[test]
fn migration_export_dry_run_and_restore_round_trip_without_secrets() {
    let root = tempfile::tempdir().unwrap();
    json(&run(root.path(), &["init", "--non-interactive"]));
    for args in [
        &["todo", "add", "--title", "migration-round-trip"][..],
        &["secret", "set", "smoke", "dummy-credential-value"][..],
    ] {
        let output = run(root.path(), args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let plugin_dir = root.path().join("plugins.d/rss");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("config.toml"),
        "[rss]\ninterval_secs = 61\ntoken = \"dummy-plugin-token\"\n",
    )
    .unwrap();

    let bundle = root.path().join("bundle");
    let export = json(&run(
        root.path(),
        &["config", "export", "--out", bundle.to_str().unwrap()],
    ));
    assert_eq!(export["credentials_excluded"], 1);
    assert_eq!(export["includes_database"], true);
    assert!(
        export["required_environment"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "MINIMAX_API_KEY")
    );
    let main = std::fs::read_to_string(bundle.join("config/ru41.toml")).unwrap();
    let plugin = std::fs::read_to_string(bundle.join("config/plugins.d/rss/config.toml")).unwrap();
    assert!(main.contains("MINIMAX_API_KEY"));
    assert!(plugin.contains("interval_secs = 61"));
    assert!(!plugin.contains("dummy-plugin-token"));
    let database = std::fs::read(bundle.join("data/state.db")).unwrap();
    let secret = b"dummy-credential-value";
    assert!(
        !database
            .windows(secret.len())
            .any(|window| window == secret)
    );

    let restored = root.path().join("restored");
    let dry_run = json(&run(
        root.path(),
        &[
            "config",
            "restore",
            "--from",
            bundle.to_str().unwrap(),
            "--target",
            restored.to_str().unwrap(),
        ],
    ));
    assert_eq!(dry_run["dry_run"], true);
    assert_eq!(dry_run["would_restore"], true);
    assert!(!restored.exists());
    assert!(
        dry_run["missing_environment"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "MINIMAX_API_KEY")
    );

    let applied = json(&run(
        root.path(),
        &[
            "config",
            "restore",
            "--from",
            bundle.to_str().unwrap(),
            "--target",
            restored.to_str().unwrap(),
            "--apply",
        ],
    ));
    assert_eq!(applied["restored"], true);
    let restored_config = restored.join("ru41.toml").display().to_string();
    let todos = json(&run(
        root.path(),
        &["--config", &restored_config, "todo", "list", "all"],
    ));
    assert!(
        serde_json::to_string(&todos)
            .unwrap()
            .contains("migration-round-trip")
    );
    let credentials = run(
        root.path(),
        &["--config", &restored_config, "secret", "list"],
    );
    assert!(credentials.status.success());
    let credentials = String::from_utf8(credentials.stdout).unwrap();
    assert!(!credentials.contains("smoke"));
    assert!(!credentials.contains("cred_"));

    let conflict = run(
        root.path(),
        &[
            "config",
            "restore",
            "--from",
            bundle.to_str().unwrap(),
            "--target",
            restored.to_str().unwrap(),
            "--apply",
        ],
    );
    assert!(!conflict.status.success());
}

#[test]
fn migration_restore_rejects_newer_config_without_writing() {
    let root = tempfile::tempdir().unwrap();
    json(&run(root.path(), &["init", "--non-interactive"]));
    let bundle = root.path().join("bundle");
    json(&run(
        root.path(),
        &["config", "export", "--out", bundle.to_str().unwrap()],
    ));
    let manifest = bundle.join("manifest.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("config_version = 1", "config_version = 2");
    std::fs::write(manifest, text).unwrap();
    let target = root.path().join("must-not-exist");
    let output = run(
        root.path(),
        &[
            "config",
            "restore",
            "--from",
            bundle.to_str().unwrap(),
            "--target",
            target.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("config_version 2")
    );
    assert!(!target.exists());
}

fn fake_codex(root: &Path) -> PathBuf {
    let path = root.join("fake-codex");
    let marker = root.join("invocations.log");
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> '{}'
if [ "${{1:-}}" = '--version' ]; then
  echo 'codex-cli fake'
  exit 0
fi
output=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--output-last-message' ]; then shift; output="$1"; fi
  shift
done
cat > /dev/null
printf '%s\n' '{{"type":"turn.completed"}}'
printf '%s' '{{"summary":"ok"}}' > "$output"
"#,
            marker.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn write_fake_config(root: &Path, bin: &Path) {
    std::fs::write(
        root.join("ru41.toml"),
        format!(
            "[store]\nkind = \"sqlite\"\npath = \".ru41/state.db\"\n[worker]\nruns_dir = \".ru41/runs\"\n[agent]\ndefault_backend = \"codex\"\n[agent.codex]\nbin = \"{}\"\n",
            bin.display()
        ),
    )
    .unwrap();
}

#[test]
fn doctor_is_offline_by_default_and_deep_runs_fake_backend() {
    let root = tempfile::tempdir().unwrap();
    let bin = fake_codex(root.path());
    write_fake_config(root.path(), &bin);
    let config = config_arg(root.path());
    let output = json(&run(root.path(), &["--config", &config, "doctor"]));
    assert_eq!(output["ok"], true);
    assert_eq!(output["model_invoked"], false);
    let invocations = std::fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert_eq!(invocations.lines().count(), 1);
    assert_eq!(invocations.trim(), "--version");

    json(&run(
        root.path(),
        &["--config", &config, "init", "--non-interactive"],
    ));
    let output = json(&run(
        root.path(),
        &["--config", &config, "doctor", "--deep"],
    ));
    assert_eq!(output["ok"], true);
    assert_eq!(output["model_invoked"], true);
    assert!(
        output["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| { check["name"] == "model_smoke" && check["status"] == "ok" })
    );
}
