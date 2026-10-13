-- PostgreSQL 版任务与执行尝试 schema，语义与 SQLite v3 一致。
CREATE TABLE IF NOT EXISTS tasks (
    id BIGSERIAL PRIMARY KEY,
    request_key TEXT NOT NULL UNIQUE,
    prompt TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued','running','retry_scheduled','succeeded','failed','cancelled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempt_count BIGINT NOT NULL DEFAULT 0,
    max_attempts BIGINT NOT NULL DEFAULT 3,
    active_attempt_id BIGINT,
    lease_until BIGINT,
    available_at BIGINT NOT NULL DEFAULT 0,
    result_json TEXT,
    last_error TEXT,
    backend_id TEXT NOT NULL DEFAULT 'codex',
    contract TEXT NOT NULL DEFAULT 'analysis_v1'
);
CREATE TABLE IF NOT EXISTS attempts (
    id BIGSERIAL PRIMARY KEY,
    task_id BIGINT NOT NULL REFERENCES tasks(id),
    worker_id TEXT NOT NULL,
    status TEXT NOT NULL
        CHECK (status IN ('running','succeeded','failed','cancelled','interrupted')),
    started_at BIGINT NOT NULL,
    finished_at BIGINT,
    run_dir TEXT,
    session_id TEXT,
    result_json TEXT,
    error TEXT,
    backend_id TEXT NOT NULL DEFAULT 'codex',
    model TEXT,
    contract TEXT NOT NULL DEFAULT 'analysis_v1'
);
CREATE INDEX IF NOT EXISTS tasks_ready ON tasks(status, available_at, id);
CREATE INDEX IF NOT EXISTS attempts_task ON attempts(task_id, id);
CREATE INDEX IF NOT EXISTS tasks_backend_ready ON tasks(backend_id, contract, status, available_at, id);
