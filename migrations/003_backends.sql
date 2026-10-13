ALTER TABLE tasks ADD COLUMN backend_id TEXT NOT NULL DEFAULT 'codex';
ALTER TABLE tasks ADD COLUMN contract TEXT NOT NULL DEFAULT 'analysis_v1';
ALTER TABLE attempts RENAME COLUMN thread_id TO session_id;
ALTER TABLE attempts ADD COLUMN backend_id TEXT NOT NULL DEFAULT 'codex';
ALTER TABLE attempts ADD COLUMN model TEXT;
ALTER TABLE attempts ADD COLUMN contract TEXT NOT NULL DEFAULT 'analysis_v1';
CREATE INDEX tasks_backend_ready ON tasks(backend_id, contract, status, available_at, id);
PRAGMA user_version = 3;
