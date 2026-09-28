-- Correlation id of the line (meta.request_id, else meta.correlation_id, else meta.trace_id), so
-- every line of one request can be listed or grouped without scanning meta_json.
-- Filled on insert by db::logs (same expression as below); this backfills existing rows once.
ALTER TABLE logs ADD COLUMN trace_id TEXT;

UPDATE logs
SET trace_id = substr(CAST(COALESCE(
        json_extract(meta_json, '$.request_id'),
        json_extract(meta_json, '$.correlation_id'),
        json_extract(meta_json, '$.trace_id')) AS TEXT), 1, 128)
WHERE meta_json IS NOT NULL AND json_valid(meta_json);

UPDATE logs SET trace_id = NULL WHERE trace_id = '';

CREATE INDEX IF NOT EXISTS idx_logs_trace
    ON logs(trace_id, timestamp_ms)
    WHERE trace_id IS NOT NULL;
