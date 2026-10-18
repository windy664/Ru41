use crate::{
    adapters::database::{postgres::PostgresTaskStore, sqlite::SqliteTaskStore},
    bootstrap::config::Config,
    ports::storage::TaskStore,
};
use anyhow::{Context, Result};
use std::{path::PathBuf, sync::Arc};

pub type StoreOptions = (String, PathBuf, Option<String>);

pub fn resolve(
    cli_kind: Option<String>,
    cli_db: Option<PathBuf>,
    cli_pg_url: Option<String>,
    config: &Config,
) -> StoreOptions {
    let kind = cli_kind
        .or_else(|| config.store.kind.clone())
        .unwrap_or_else(|| "sqlite".into());
    let db = cli_db
        .or_else(|| config.store.path.clone())
        .unwrap_or_else(|| PathBuf::from(".ru41/state.db"));
    let pg_url = cli_pg_url.or_else(|| config.store.url.clone());
    (kind, db, pg_url)
}

pub fn open_store((kind, db, pg_url): &StoreOptions) -> Result<Arc<dyn TaskStore>> {
    match kind.as_str() {
        "sqlite" => Ok(Arc::new(SqliteTaskStore::open(db)?)),
        "postgres" => {
            let url = pg_url.as_deref().context(
                "--store postgres requires --pg-url, RU41_PG_URL or store.url in config",
            )?;
            Ok(Arc::new(tokio::task::block_in_place(|| {
                PostgresTaskStore::open(url)
            })?))
        }
        other => anyhow::bail!("unknown store backend: {other}"),
    }
}
