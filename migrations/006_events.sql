-- 事件与触发它的状态变更共享事务；只保存状态和资源引用。
CREATE TABLE events (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 version INTEGER NOT NULL DEFAULT 1,
 kind TEXT NOT NULL,
 occurred_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
 resource_id TEXT NOT NULL,
 task_id INTEGER,
 attempt_id INTEGER,
 run_id TEXT,
 status TEXT NOT NULL
);
CREATE TRIGGER task_event_insert AFTER INSERT ON tasks BEGIN
 INSERT INTO events(kind,resource_id,task_id,attempt_id,status)
 VALUES ('task.' || NEW.status, CAST(NEW.id AS TEXT), NEW.id, NEW.active_attempt_id, NEW.status);
END;
CREATE TRIGGER task_event_update AFTER UPDATE OF status ON tasks
WHEN OLD.status <> NEW.status BEGIN
 INSERT INTO events(kind,resource_id,task_id,attempt_id,run_id,status)
 VALUES ('task.' || NEW.status, CAST(NEW.id AS TEXT), NEW.id, COALESCE(NEW.active_attempt_id,OLD.active_attempt_id),
 CASE WHEN COALESCE(NEW.active_attempt_id,OLD.active_attempt_id) IS NOT NULL
 THEN 'run-' || NEW.id || '-' || COALESCE(NEW.active_attempt_id,OLD.active_attempt_id) END, NEW.status);
END;
CREATE TRIGGER interaction_event_insert AFTER INSERT ON records
WHEN NEW.kind='interaction' AND json_extract(NEW.body,'$.status') IN ('pending','answered','expired') BEGIN
 INSERT INTO events(kind, resource_id, task_id, attempt_id, run_id, status) VALUES (
 CASE json_extract(NEW.body,'$.status') WHEN 'pending' THEN 'interaction.requested'
 WHEN 'answered' THEN 'interaction.answered' ELSE 'interaction.expired' END,
 NEW.id,
 (SELECT json_extract(body,'$.task_id') FROM records WHERE kind='run' AND id=json_extract(NEW.body,'$.run_id')),
 (SELECT json_extract(body,'$.attempt_id') FROM records WHERE kind='run' AND id=json_extract(NEW.body,'$.run_id')),
 json_extract(NEW.body,'$.run_id'), json_extract(NEW.body,'$.status'));
END;
CREATE TRIGGER interaction_event_update AFTER UPDATE OF body ON records
WHEN NEW.kind='interaction' AND json_extract(OLD.body,'$.status') IS NOT json_extract(NEW.body,'$.status') AND json_extract(NEW.body,'$.status') IN ('pending','answered','expired') BEGIN
 INSERT INTO events(kind, resource_id, task_id, attempt_id, run_id, status) VALUES (
 CASE json_extract(NEW.body,'$.status') WHEN 'pending' THEN 'interaction.requested'
 WHEN 'answered' THEN 'interaction.answered' ELSE 'interaction.expired' END,
 NEW.id,
 (SELECT json_extract(body,'$.task_id') FROM records WHERE kind='run' AND id=json_extract(NEW.body,'$.run_id')),
 (SELECT json_extract(body,'$.attempt_id') FROM records WHERE kind='run' AND id=json_extract(NEW.body,'$.run_id')),
 json_extract(NEW.body,'$.run_id'), json_extract(NEW.body,'$.status'));
END;
PRAGMA user_version = 6;
