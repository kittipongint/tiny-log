CREATE TABLE IF NOT EXISTS logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp_ms INTEGER NOT NULL,
    app TEXT NOT NULL,
    level TEXT NOT NULL,
    source TEXT,
    message TEXT NOT NULL,
    meta_json TEXT
);

CREATE INDEX IF NOT EXISTS idx_logs_timestamp
    ON logs(timestamp_ms DESC);

CREATE INDEX IF NOT EXISTS idx_logs_app_timestamp
    ON logs(app, timestamp_ms DESC);

CREATE INDEX IF NOT EXISTS idx_logs_level_timestamp
    ON logs(level, timestamp_ms DESC);
