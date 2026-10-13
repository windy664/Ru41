ALTER TABLE tasks RENAME TO tasks_v1;
CREATE TABLE tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_key TEXT NOT NULL UNIQUE,
    prompt TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_scheduled','succeeded','failed','cancelled')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    active_attempt_id INTEGER,
    lease_until INTEGER,
    available_at INTEGER NOT NULL DEFAULT 0,
    result_json TEXT,
    last_error TEXT
);
INSERT INTO tasks(id,request_key,prompt,status,created_at,updated_at)
    SELECT id,request_key,prompt,status,created_at,updated_at FROM tasks_v1;
DROP TABLE tasks_v1;
CREATE TABLE attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id INTEGER NOT NULL REFERENCES tasks(id),
    worker_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','succeeded','failed','cancelled','interrupted')),
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    run_dir TEXT,
    thread_id TEXT,
    result_json TEXT,
    error TEXT
);
CREATE INDEX tasks_ready ON tasks(status, available_at, id);
CREATE INDEX attempts_task ON attempts(task_id, id);
PRAGMA user_version = 2;
