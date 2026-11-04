-- 底座上报的 token 用量（codex turn.completed 的 usage）；NULL = 底座未上报。
ALTER TABLE attempts ADD COLUMN input_tokens BIGINT;
ALTER TABLE attempts ADD COLUMN cached_input_tokens BIGINT;
ALTER TABLE attempts ADD COLUMN output_tokens BIGINT;
ALTER TABLE attempts ADD COLUMN total_tokens BIGINT;
CREATE INDEX attempts_usage_time ON attempts(started_at) WHERE total_tokens IS NOT NULL;
