use super::OpsOutcome;
use crate::{
    bootstrap::{
        config::{CURRENT_CONFIG_VERSION, Config},
        store,
    },
    ingress::cli::Cli,
    plugins::{API_CRATE_VERSION, PLUGIN_API_VERSION},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

const FORMAT_VERSION: u32 = 1;
const DATABASE_SCHEMA_VERSION: i64 = 8;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u32,
    created_at: String,
    config_version: u32,
    ru41_version: String,
    plugin_api_version: String,
    plugin_abi_version: u32,
    database_kind: String,
    database_schema_version: Option<i64>,
    includes_database: bool,
    credentials_excluded: usize,
    configured_plugins: Vec<String>,
    plugin_binaries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct RequiredSecret {
    env: String,
    source: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredSecrets {
    #[serde(default)]
    required: Vec<RequiredSecret>,
    #[serde(default)]
    manual_fields: Vec<String>,
}

#[derive(Debug)]
struct PluginConfig {
    name: String,
    file_name: &'static str,
    value: toml::Value,
}

pub(crate) fn export(cli: &Cli, out: &Path) -> OpsOutcome {
    match export_bundle(cli, out) {
        Ok(report) => OpsOutcome::success(report),
        Err(error) => OpsOutcome::failure(format!("{error:#}")),
    }
}

pub(crate) fn restore(from: &Path, target: &Path, apply: bool) -> OpsOutcome {
    match restore_bundle(from, target, apply) {
        Ok(report) => OpsOutcome::success(report),
        Err(error) => OpsOutcome::failure(format!("{error:#}")),
    }
}

fn export_bundle(cli: &Cli, out: &Path) -> Result<serde_json::Value> {
    ensure!(
        !out.exists(),
        "export target already exists: {}",
        out.display()
    );
    let (config, config_path) = Config::load(cli.config.as_deref())?;
    let config_path = config_path.context("migration export requires a configuration file")?;
    let main_text = std::fs::read_to_string(&config_path)?;
    let main_raw: toml::Value = toml::from_str(&main_text)?;
    let plugin_root = std::env::var_os("RU41_PLUGIN_CONFIG_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Config::default_plugin_config_root(Some(&config_path), Path::new(".")));

    let mut secrets = RequiredSecrets::default();
    collect_environment_references(&main_raw, "", &mut secrets.required);
    let mut main_portable = portable_value(main_raw.clone(), "", &mut secrets)?;
    main_portable
        .as_table_mut()
        .context("configuration root is not a TOML table")?
        .insert(
            "config_version".into(),
            toml::Value::Integer(i64::from(CURRENT_CONFIG_VERSION)),
        );
    let plugin_configs = read_plugin_configs(&plugin_root)?;
    let mut portable_plugins = Vec::new();
    for plugin in plugin_configs {
        collect_environment_references(&plugin.value, "sources", &mut secrets.required);
        let portable = portable_value(plugin.value, "sources", &mut secrets)?;
        portable_plugins.push(PluginConfig {
            value: portable,
            ..plugin
        });
    }
    add_known_environment_requirements(&config, &mut secrets.required);
    secrets.required.sort();
    secrets.required.dedup();
    secrets.manual_fields.sort();
    secrets.manual_fields.dedup();

    let parent = out.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let staging = temporary_sibling(out, "export")?;
    let result = (|| -> Result<(Manifest, Option<i64>)> {
        create_private_dir(&staging)?;
        create_private_dir(&staging.join("config"))?;
        create_private_dir(&staging.join("versions"))?;
        write_private(
            &staging.join("config/ru41.toml"),
            toml::to_string_pretty(&main_portable)?.as_bytes(),
        )?;
        if !portable_plugins.is_empty() {
            create_private_dir(&staging.join("config/plugins.d"))?;
        }
        for plugin in &portable_plugins {
            let dir = staging.join("config/plugins.d").join(&plugin.name);
            create_private_dir(&dir)?;
            write_private(
                &dir.join(plugin.file_name),
                toml::to_string_pretty(&plugin.value)?.as_bytes(),
            )?;
        }

        let options = store::resolve(
            cli.store.clone(),
            cli.db.clone(),
            cli.pg_url.clone(),
            &config,
        );
        let mut database_schema_version = None;
        let mut credentials_excluded = 0;
        let includes_database = options.0 == "sqlite";
        if includes_database {
            ensure!(
                options.1.is_file(),
                "SQLite database does not exist: {}",
                options.1.display()
            );
            create_private_dir(&staging.join("data"))?;
            let database = staging.join("data/state.db");
            online_backup(&options.1, &database)?;
            credentials_excluded = exclude_credentials(&database)?;
            database_schema_version = Some(validate_database(&database)?);
        } else {
            ensure!(
                options.0 == "postgres",
                "unsupported store backend: {}",
                options.0
            );
        }

        let mut configured_plugins: Vec<String> =
            config.sources.keys().map(str::to_owned).collect();
        configured_plugins.sort();
        let plugin_binaries = plugin_binaries(&config.plugins.dir_or_default())?;
        let manifest = Manifest {
            format_version: FORMAT_VERSION,
            created_at: chrono::Utc::now().to_rfc3339(),
            config_version: CURRENT_CONFIG_VERSION,
            ru41_version: env!("CARGO_PKG_VERSION").into(),
            plugin_api_version: API_CRATE_VERSION.into(),
            plugin_abi_version: PLUGIN_API_VERSION,
            database_kind: options.0,
            database_schema_version,
            includes_database,
            credentials_excluded,
            configured_plugins,
            plugin_binaries,
        };
        write_private(
            &staging.join("manifest.toml"),
            toml::to_string_pretty(&manifest)?.as_bytes(),
        )?;
        write_private(
            &staging.join("secrets.required.toml"),
            toml::to_string_pretty(&secrets)?.as_bytes(),
        )?;
        write_private(
            &staging.join("versions/ru41.txt"),
            format!("{}\n", manifest.ru41_version).as_bytes(),
        )?;
        write_private(
            &staging.join("versions/plugin-api.txt"),
            format!("{}\n", manifest.plugin_api_version).as_bytes(),
        )?;
        let mut plugin_lines = manifest.configured_plugins.clone();
        for binary in &manifest.plugin_binaries {
            if !plugin_lines.contains(binary) {
                plugin_lines.push(binary.clone());
            }
        }
        plugin_lines.sort();
        plugin_lines.dedup();
        write_private(
            &staging.join("versions/plugins.txt"),
            format!("{}\n", plugin_lines.join("\n")).as_bytes(),
        )?;
        Ok((manifest, database_schema_version))
    })();
    let (manifest, schema) = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    std::fs::rename(&staging, out)
        .with_context(|| format!("cannot move migration bundle into place: {}", out.display()))?;
    Ok(json!({
        "bundle": out,
        "format_version": FORMAT_VERSION,
        "includes_database": manifest.includes_database,
        "database_schema_version": schema,
        "credentials_excluded": manifest.credentials_excluded,
        "required_environment": secrets.required.iter().map(|item| &item.env).collect::<Vec<_>>(),
        "manual_secret_fields": secrets.manual_fields,
        "database_notice": if manifest.includes_database {
            "state.db contains private tasks and bindings; protect the whole bundle"
        } else {
            "PostgreSQL data is external and is not included"
        },
    }))
}

fn restore_bundle(from: &Path, target: &Path, apply: bool) -> Result<serde_json::Value> {
    let manifest: Manifest = read_toml(&from.join("manifest.toml"))?;
    ensure!(
        manifest.format_version == FORMAT_VERSION,
        "migration format {} is newer than supported version {FORMAT_VERSION}",
        manifest.format_version
    );
    ensure!(
        manifest.plugin_api_version == API_CRATE_VERSION,
        "plugin-api version mismatch: bundle {} / current {API_CRATE_VERSION}; use matching Ru41 binaries and plugins",
        manifest.plugin_api_version
    );
    ensure!(
        manifest.plugin_abi_version == PLUGIN_API_VERSION,
        "plugin ABI mismatch: bundle {} / current {PLUGIN_API_VERSION}",
        manifest.plugin_abi_version
    );
    ensure!(
        manifest.config_version <= CURRENT_CONFIG_VERSION,
        "config_version {} is newer than supported version {CURRENT_CONFIG_VERSION}",
        manifest.config_version
    );
    let main_path = from.join("config/ru41.toml");
    let main_value: toml::Value = read_toml(&main_path)?;
    let plugin_configs = read_plugin_configs(&from.join("config/plugins.d"))?;
    Config::load_from_with_roots(
        Some(&main_path),
        &from.join("config"),
        None,
        None,
        Some(&from.join("config/plugins.d")),
    )?;
    let secrets: RequiredSecrets = read_toml(&from.join("secrets.required.toml"))?;
    if manifest.includes_database {
        ensure!(
            manifest.database_kind == "sqlite",
            "only SQLite database bundles can be restored"
        );
        let schema = validate_database(&from.join("data/state.db"))?;
        ensure!(
            schema <= DATABASE_SCHEMA_VERSION,
            "database schema {schema} is newer than supported version {DATABASE_SCHEMA_VERSION}"
        );
        let credentials = credential_count(&from.join("data/state.db"))?;
        ensure!(
            credentials == 0,
            "migration bundle must not contain credential records"
        );
    }
    ensure!(
        !target.exists(),
        "restore target already exists; refusing to overwrite: {}",
        target.display()
    );
    let missing_environment = missing_environment(&secrets.required);
    let version_warning = (manifest.ru41_version != env!("CARGO_PKG_VERSION")).then(|| {
        format!(
            "bundle Ru41 {} differs from current {}; database and config compatibility checks passed",
            manifest.ru41_version,
            env!("CARGO_PKG_VERSION")
        )
    });
    if !apply {
        return Ok(json!({
            "dry_run": true,
            "bundle": from,
            "target": target,
            "config_version": manifest.config_version,
            "database_schema_version": manifest.database_schema_version,
            "missing_environment": missing_environment,
            "manual_secret_fields": secrets.manual_fields,
            "version_warning": version_warning,
            "would_restore": true,
            "external_dependencies": external_dependencies(),
        }));
    }

    create_private_dir(target)?;
    let result = (|| -> Result<()> {
        create_private_dir(&target.join("data"))?;
        let mut restored_main = main_value;
        rewrite_data_paths(&mut restored_main, target, &manifest.database_kind)?;
        write_private(
            &target.join("ru41.toml"),
            toml::to_string_pretty(&restored_main)?.as_bytes(),
        )?;
        if !plugin_configs.is_empty() {
            create_private_dir(&target.join("plugins.d"))?;
        }
        for plugin in plugin_configs {
            let dir = target.join("plugins.d").join(plugin.name);
            create_private_dir(&dir)?;
            write_private(
                &dir.join(plugin.file_name),
                toml::to_string_pretty(&plugin.value)?.as_bytes(),
            )?;
        }
        if manifest.includes_database {
            copy_private(&from.join("data/state.db"), &target.join("data/state.db"))?;
        }
        create_private_dir(&target.join("data/runs"))?;
        Config::load_from_with_roots(
            Some(&target.join("ru41.toml")),
            target,
            None,
            Some(target),
            Some(&target.join("plugins.d")),
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(target);
        return Err(error);
    }
    Ok(json!({
        "dry_run": false,
        "restored": true,
        "target": target,
        "missing_environment": missing_environment,
        "manual_secret_fields": secrets.manual_fields,
        "version_warning": version_warning,
        "external_dependencies": external_dependencies(),
        "next_steps": [
            "export the missing environment variables on this machine",
            "re-enter excluded credential-manager values if their references are still needed",
            "clone the Vault and blog repositories separately",
            "set RU41_HOME to the restore target and run ru41 doctor"
        ],
    }))
}

fn read_plugin_configs(root: &Path) -> Result<Vec<PluginConfig>> {
    let mut configs = Vec::new();
    if !root.exists() {
        return Ok(configs);
    }
    ensure!(root.is_dir(), "not a directory: {}", root.display());
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    for dir in dirs {
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .context("plugin config directory name is not UTF-8")?
            .to_owned();
        ensure!(
            !name.is_empty() && name != "." && name != ".." && !name.contains('/'),
            "invalid plugin config directory name"
        );
        for file_name in ["config.toml", "config.local.toml"] {
            let path = dir.join(file_name);
            if path.exists() {
                ensure!(path.is_file(), "not a file: {}", path.display());
                configs.push(PluginConfig {
                    name: name.clone(),
                    file_name,
                    value: read_toml(&path)?,
                });
            }
        }
    }
    Ok(configs)
}

fn portable_value(
    mut value: toml::Value,
    prefix: &str,
    secrets: &mut RequiredSecrets,
) -> Result<toml::Value> {
    fn visit(value: &mut toml::Value, prefix: &str, secrets: &mut RequiredSecrets) {
        match value {
            toml::Value::Table(table) => {
                let keys: Vec<String> = table.keys().cloned().collect();
                for key in keys {
                    let path = join_path(prefix, &key);
                    if key == "extra_env" && prefix == "agent.pi" {
                        if let Some(values) = table
                            .remove(&key)
                            .and_then(|value| value.as_table().cloned())
                        {
                            for env in values.keys() {
                                secrets.required.push(RequiredSecret {
                                    env: env.clone(),
                                    source: path.clone(),
                                });
                            }
                        }
                        continue;
                    }
                    if is_sensitive_key(&key) {
                        table.remove(&key);
                        if let Some(env) = environment_for(&path) {
                            secrets.required.push(RequiredSecret {
                                env: env.into(),
                                source: path.clone(),
                            });
                        } else {
                            secrets.manual_fields.push(path.clone());
                        }
                        continue;
                    }
                    if key == "config_overrides"
                        && let Some(items) = table.get_mut(&key).and_then(toml::Value::as_array_mut)
                    {
                        items.retain(|item| {
                            let Some(text) = item.as_str() else {
                                return true;
                            };
                            ![
                                "api_key", "apikey", "password", "secret", "token", "cookie",
                                "openid",
                            ]
                            .iter()
                            .any(|needle| text.to_ascii_lowercase().contains(needle))
                        });
                    }
                    if let Some(value) = table.get_mut(&key) {
                        visit(value, &path, secrets);
                    }
                }
            }
            toml::Value::Array(items) => {
                for item in items {
                    visit(item, prefix, secrets);
                }
            }
            _ => {}
        }
    }
    visit(&mut value, prefix, secrets);
    Ok(value)
}

fn collect_environment_references(
    value: &toml::Value,
    prefix: &str,
    required: &mut Vec<RequiredSecret>,
) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                let path = join_path(prefix, key);
                if matches!(key.as_str(), "api_key_env" | "env_key")
                    && let Some(env) = value.as_str()
                    && valid_environment_name(env)
                {
                    required.push(RequiredSecret {
                        env: env.into(),
                        source: path.clone(),
                    });
                }
                collect_environment_references(value, &path, required);
            }
        }
        toml::Value::Array(items) => {
            for item in items {
                collect_environment_references(item, prefix, required);
            }
        }
        _ => {}
    }
}

fn add_known_environment_requirements(config: &Config, required: &mut Vec<RequiredSecret>) {
    let sources = config.sources.to_toml();
    for name in sources.keys() {
        let variables: &[&str] = match name.as_str() {
            "ecard" => &["RU41_ECARD_TOKEN"],
            "jwxt" => &["RU41_JWXT_USERNAME", "RU41_JWXT_PASSWORD"],
            "library" => &["RU41_LIBRARY_USERNAME", "RU41_LIBRARY_PASSWORD"],
            "chaoxing" => &["RU41_XXT_PHONE", "RU41_XXT_PASSWORD"],
            "compshare" => &["RU41_COMPSHARE_COOKIE"],
            "teachermate" => &["RU41_WZJ_OPENID"],
            _ => &[],
        };
        for env in variables {
            required.push(RequiredSecret {
                env: (*env).into(),
                source: format!("sources.{name}"),
            });
        }
    }
}

fn environment_for(path: &str) -> Option<&'static str> {
    Some(match path {
        "store.url" => "RU41_PG_URL",
        "api.token" => "RU41_API_TOKEN",
        "channel.qq.app_id" => "RU41_QQ_APP_ID",
        "channel.qq.app_secret" => "RU41_QQ_APP_SECRET",
        "channel.qq.bind_secret" => "RU41_QQ_BIND_SECRET",
        "channel.qq.default_notify_target" => "RU41_TODO_NOTIFY_TARGET",
        "channel.feishu.webhook_url" => "RU41_FEISHU_WEBHOOK_URL",
        "sources.ecard.token" => "RU41_ECARD_TOKEN",
        "sources.jwxt.username" => "RU41_JWXT_USERNAME",
        "sources.jwxt.password" => "RU41_JWXT_PASSWORD",
        "sources.library.username" => "RU41_LIBRARY_USERNAME",
        "sources.library.password" => "RU41_LIBRARY_PASSWORD",
        "sources.chaoxing.phone" => "RU41_XXT_PHONE",
        "sources.chaoxing.password" => "RU41_XXT_PASSWORD",
        "sources.compshare.cookie" => "RU41_COMPSHARE_COOKIE",
        "sources.teachermate.openid" => "RU41_WZJ_OPENID",
        _ => return None,
    })
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "username"
            | "phone"
            | "password"
            | "token"
            | "secret"
            | "cookie"
            | "openid"
            | "app_id"
            | "app_secret"
            | "bind_secret"
            | "webhook_url"
            | "default_notify_target"
            | "url"
            | "api_key"
            | "access_key"
            | "private_key"
    ) || key.ends_with("_token")
        || key.ends_with("_password")
        || key.ends_with("_secret")
        || key.ends_with("_cookie")
        || key.ends_with("_openid")
}

fn valid_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn join_path(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.into()
    } else {
        format!("{prefix}.{key}")
    }
}

fn online_backup(source: &Path, destination: &Path) -> Result<()> {
    let connection = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("cannot open SQLite database {}", source.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(30))?;
    connection.execute(
        "VACUUM INTO ?1",
        params![destination.to_string_lossy().as_ref()],
    )?;
    set_private_permissions(destination)?;
    Ok(())
}

fn exclude_credentials(database: &Path) -> Result<usize> {
    let connection = Connection::open(database)?;
    connection.busy_timeout(std::time::Duration::from_secs(30))?;
    let excluded = connection.execute(
        "DELETE FROM records WHERE kind IN ('credential', 'credential_audit')",
        [],
    )?;
    connection.execute_batch("VACUUM")?;
    set_private_permissions(database)?;
    Ok(excluded)
}

fn validate_database(path: &Path) -> Result<i64> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("cannot open bundled database {}", path.display()))?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(
        integrity == "ok",
        "SQLite integrity_check failed: {integrity}"
    );
    let schema = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(schema)
}

fn credential_count(path: &Path) -> Result<usize> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let count = connection.query_row(
        "SELECT COUNT(*) FROM records WHERE kind IN ('credential', 'credential_audit')",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    usize::try_from(count).context("invalid credential count")
}

fn plugin_binaries(dir: &Path) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|extension| extension == "so")
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            {
                names.insert(stem.strip_prefix("libru41_plugin_").unwrap_or(stem).into());
            }
        }
    }
    Ok(names.into_iter().collect())
}

fn missing_environment(required: &[RequiredSecret]) -> Vec<String> {
    let mut missing: Vec<String> = required
        .iter()
        .filter(|item| std::env::var_os(&item.env).is_none_or(|value| value.as_os_str().is_empty()))
        .map(|item| item.env.clone())
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

fn rewrite_data_paths(value: &mut toml::Value, target: &Path, kind: &str) -> Result<()> {
    let root = value
        .as_table_mut()
        .context("configuration root must be a table")?;
    let store = root
        .entry("store")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .context("store must be a table")?;
    store.insert("kind".into(), toml::Value::String(kind.into()));
    if kind == "sqlite" {
        store.insert(
            "path".into(),
            toml::Value::String(target.join("data/state.db").display().to_string()),
        );
    }
    let worker = root
        .entry("worker")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .context("worker must be a table")?;
    worker.insert(
        "runs_dir".into(),
        toml::Value::String(target.join("data/runs").display().to_string()),
    );
    Ok(())
}

fn external_dependencies() -> Vec<&'static str> {
    vec![
        "Vault and blog repositories are separate Git clones",
        "plugin .so files must match this machine and plugin-api version",
        "browser, ASR and other sidecars are configured separately",
        "runs, knowledge.db and embedding caches are intentionally excluded",
    ]
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("invalid TOML in {}", path.display()))
}

fn temporary_sibling(path: &Path, operation: &str) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("migration path must have a UTF-8 file name")?;
    let temporary = path.with_file_name(format!(".{name}.{operation}-{}.tmp", std::process::id()));
    ensure!(
        !temporary.exists(),
        "temporary migration path already exists: {}",
        temporary.display()
    );
    Ok(temporary)
}

fn create_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir(path).with_context(|| format!("cannot create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

fn copy_private(source: &Path, destination: &Path) -> Result<()> {
    std::fs::copy(source, destination)?;
    set_private_permissions(destination)
}

fn set_private_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_config_drops_secret_values_but_keeps_environment_names() {
        let mut secrets = RequiredSecrets::default();
        let value: toml::Value = toml::from_str(
            r#"
[agent.pi]
api_key_env = "MINIMAX_API_KEY"
model = "minimax/MiniMax-M3"
[channel.qq]
app_secret = "do-not-export"
default_notify_target = "user:do-not-export"
[sources.rss]
feeds = ["https://example.com/feed"]
token = "do-not-export"
"#,
        )
        .unwrap();
        collect_environment_references(&value, "", &mut secrets.required);
        let portable = portable_value(value, "", &mut secrets).unwrap();
        let text = toml::to_string(&portable).unwrap();
        assert!(!text.contains("do-not-export"));
        assert!(text.contains("MINIMAX_API_KEY"));
        assert!(text.contains("https://example.com/feed"));
        assert_eq!(secrets.required[0].env, "MINIMAX_API_KEY");
        assert!(secrets.manual_fields.contains(&"sources.rss.token".into()));
    }
}
