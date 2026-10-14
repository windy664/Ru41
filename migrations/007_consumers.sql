-- 同一消费者顺序消费；不同消费者互不共享进度。
CREATE TABLE event_consumers (
 name TEXT PRIMARY KEY,
 start_after BIGINT NOT NULL,
 cursor BIGINT NOT NULL,
 max_attempts BIGINT NOT NULL,
 attempts BIGINT NOT NULL DEFAULT 0,
 generation BIGINT NOT NULL DEFAULT 0,
 active_event_id BIGINT REFERENCES events(id),
 lease_until BIGINT,
 available_at BIGINT NOT NULL DEFAULT 0,
 blocked BOOLEAN NOT NULL DEFAULT FALSE,
 last_failure TEXT
);
PRAGMA user_version = 7;
