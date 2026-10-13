-- 任务完成后的渠道通知路由（QQ 为首个渠道）。
ALTER TABLE tasks ADD COLUMN notify_channel TEXT;
ALTER TABLE tasks ADD COLUMN notify_target TEXT;
ALTER TABLE tasks ADD COLUMN notify_msg_id TEXT;
ALTER TABLE tasks ADD COLUMN notified_at TEXT;
CREATE INDEX tasks_notify ON tasks(notify_channel, notified_at, status, id);
PRAGMA user_version = 4;
