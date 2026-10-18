use ru41::adapters::database::{
    TaskStatusCounts, postgres::PostgresTaskStore, sqlite::SqliteTaskStore,
};

const STATUSES: [&str; 6] = [
    "queued",
    "running",
    "retry_scheduled",
    "succeeded",
    "failed",
    "cancelled",
];

fn populated_counts() -> TaskStatusCounts {
    TaskStatusCounts {
        queued: 2,
        running: 1,
        retry_scheduled: 1,
        succeeded: 1,
        failed: 1,
        cancelled: 1,
    }
}

#[test]
fn sqlite_empty_database_has_zero_status_counts() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state.db")).unwrap();
    let expected = TaskStatusCounts::default();

    assert_eq!(store.task_status_counts().unwrap(), expected);
    assert_eq!(
        serde_json::to_value(expected).unwrap(),
        serde_json::json!({
            "queued": 0,
            "running": 0,
            "retry_scheduled": 0,
            "succeeded": 0,
            "failed": 0,
            "cancelled": 0,
        })
    );
}

#[test]
fn sqlite_status_counts_are_aggregated_for_every_status() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = SqliteTaskStore::open(&path).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    for (index, status) in STATUSES.into_iter().enumerate() {
        connection
            .execute(
                "INSERT INTO tasks(request_key,prompt,status) VALUES (?1,?2,?3)",
                rusqlite::params![format!("request-{index}"), "prompt", status],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO tasks(request_key,prompt,status) VALUES ('extra-queued','prompt','queued')",
            [],
        )
        .unwrap();

    assert_eq!(store.task_status_counts().unwrap(), populated_counts());
}

fn postgres_store() -> Option<PostgresTaskStore> {
    let Ok(url) = std::env::var("RU41_TEST_PG_URL") else {
        eprintln!("RU41_TEST_PG_URL is not set; skipping PostgreSQL status counts test");
        return None;
    };
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    client
        .batch_execute("DROP TABLE IF EXISTS schema_migrations, attempts, tasks, records, events, event_clock, event_consumers CASCADE")
        .unwrap();
    drop(client);
    Some(PostgresTaskStore::open(&url).unwrap())
}

#[test]
fn postgres_status_counts_are_aggregated_for_every_status() {
    let Some(store) = postgres_store() else {
        return;
    };
    assert_eq!(
        store.task_status_counts().unwrap(),
        TaskStatusCounts::default()
    );

    let url = std::env::var("RU41_TEST_PG_URL").unwrap();
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    for (index, status) in STATUSES.into_iter().enumerate() {
        let request_key = format!("request-{index}");
        client
            .execute(
                "INSERT INTO tasks(request_key,prompt,status) VALUES ($1,$2,$3)",
                &[&request_key, &"prompt", &status],
            )
            .unwrap();
    }
    client
        .execute(
            "INSERT INTO tasks(request_key,prompt,status) VALUES ($1,$2,$3)",
            &[&"extra-queued", &"prompt", &"queued"],
        )
        .unwrap();
    drop(client);

    assert_eq!(store.task_status_counts().unwrap(), populated_counts());
}
