CREATE TABLE IF NOT EXISTS logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp_ms INTEGER NOT NULL,
    app TEXT NOT NULL,
    level TEXT NOT NULL,
    source TEXT,
    message TEXT NOT NULL,
    meta_json TEXT,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_logs_timestamp
    ON logs(timestamp_ms DESC);

CREATE INDEX IF NOT EXISTS idx_logs_app_timestamp
    ON logs(app, timestamp_ms DESC);

CREATE INDEX IF NOT EXISTS idx_logs_level_timestamp
    ON logs(level, timestamp_ms DESC);

CREATE TABLE IF NOT EXISTS admin_user (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    admin_id INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    FOREIGN KEY (admin_id)
        REFERENCES admin_user(id)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_sessions_expires
    ON sessions(expires_at);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
