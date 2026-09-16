CREATE TABLE IF NOT EXISTS host_samples (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp_ms INTEGER NOT NULL,
    host TEXT NOT NULL,
    cpu_pct REAL,
    mem_used_bytes INTEGER,
    mem_total_bytes INTEGER,
    disk_used_bytes INTEGER,
    disk_total_bytes INTEGER,
    load1 REAL
);

CREATE INDEX IF NOT EXISTS idx_host_samples_host_ts
    ON host_samples(host, timestamp_ms DESC);

CREATE TABLE IF NOT EXISTS service_checks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp_ms INTEGER NOT NULL,
    host TEXT NOT NULL,
    service TEXT NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    latency_ms INTEGER,
    message TEXT,
    meta_json TEXT
);

CREATE INDEX IF NOT EXISTS idx_service_checks_host_ts
    ON service_checks(host, timestamp_ms DESC);

CREATE INDEX IF NOT EXISTS idx_service_checks_service_ts
    ON service_checks(service, timestamp_ms DESC);
