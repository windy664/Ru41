-- 事务计数器持锁至提交，防止序列 ID 的提交乱序导致游标跳过事件。
CREATE TABLE event_clock (singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton), value BIGINT NOT NULL);
INSERT INTO event_clock(value) VALUES (0);
CREATE TABLE events (
 id BIGINT PRIMARY KEY,
 version INTEGER NOT NULL DEFAULT 1,
 kind TEXT NOT NULL,
 occurred_at TEXT NOT NULL DEFAULT to_char(clock_timestamp() AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
 resource_id TEXT NOT NULL,
 task_id BIGINT,
 attempt_id BIGINT,
 run_id TEXT,
 status TEXT NOT NULL
);
CREATE OR REPLACE FUNCTION ru41_task_event() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE seq BIGINT; attempt BIGINT;
BEGIN
 IF TG_OP='UPDATE' AND OLD.status=NEW.status THEN RETURN NEW; END IF;
 attempt := NEW.active_attempt_id;
 IF TG_OP='UPDATE' THEN attempt := COALESCE(attempt, OLD.active_attempt_id); END IF;
 UPDATE event_clock SET value=value+1 WHERE singleton RETURNING value INTO seq;
 INSERT INTO events(id,kind,resource_id,task_id,attempt_id,run_id,status)
 VALUES(seq,'task.' || NEW.status,NEW.id::text,NEW.id,attempt,
 CASE WHEN attempt IS NOT NULL THEN 'run-' || NEW.id || '-' || attempt END,NEW.status);
 RETURN NEW;
END $$;
CREATE TRIGGER task_event_insert AFTER INSERT ON tasks FOR EACH ROW EXECUTE FUNCTION ru41_task_event();
CREATE TRIGGER task_event_update AFTER UPDATE OF status ON tasks FOR EACH ROW EXECUTE FUNCTION ru41_task_event();
CREATE OR REPLACE FUNCTION ru41_interaction_event() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE seq BIGINT; data JSONB; run JSONB; state TEXT; event_kind TEXT;
BEGIN
 IF NEW.kind <> 'interaction' THEN RETURN NEW; END IF;
 data := NEW.body::jsonb;
 state := data->>'status';
 IF TG_OP='UPDATE' AND (OLD.body::jsonb->>'status') IS NOT DISTINCT FROM state THEN RETURN NEW; END IF;
 event_kind := CASE state WHEN 'pending' THEN 'interaction.requested'
 WHEN 'answered' THEN 'interaction.answered' WHEN 'expired' THEN 'interaction.expired' END;
 IF event_kind IS NULL THEN RETURN NEW; END IF;
 SELECT body::jsonb INTO run FROM records WHERE kind='run' AND id=data->>'run_id';
 UPDATE event_clock SET value=value+1 WHERE singleton RETURNING value INTO seq;
 INSERT INTO events(id,kind,resource_id,task_id,attempt_id,run_id,status)
 VALUES(seq,event_kind,NEW.id,(run->>'task_id')::bigint,(run->>'attempt_id')::bigint,data->>'run_id',state);
 RETURN NEW;
END $$;
CREATE TRIGGER interaction_event_insert AFTER INSERT ON records FOR EACH ROW EXECUTE FUNCTION ru41_interaction_event();
CREATE TRIGGER interaction_event_update AFTER UPDATE OF body ON records FOR EACH ROW EXECUTE FUNCTION ru41_interaction_event();
