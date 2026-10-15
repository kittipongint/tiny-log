-- The Monitor overview needs the newest row per host and per (host, service). Finding it with
-- GROUP BY over the whole retention window (millions of rows, temp_store=MEMORY) took 30 s+ in
-- prod and could push the container past its memory limit. These pointer tables hold one row per
-- host / (host, service); the triggers keep them current on every insert, so the overview reads a
-- few dozen rows and joins back by primary key.

CREATE TABLE IF NOT EXISTS host_latest (
    host TEXT PRIMARY KEY,
    sample_id INTEGER NOT NULL,
    timestamp_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS service_latest (
    host TEXT NOT NULL,
    service TEXT NOT NULL,
    check_id INTEGER NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    PRIMARY KEY (host, service)
);

-- ">=": on equal timestamps the later insert (higher id) wins.
CREATE TRIGGER IF NOT EXISTS host_samples_latest AFTER INSERT ON host_samples
BEGIN
    INSERT INTO host_latest (host, sample_id, timestamp_ms)
    VALUES (NEW.host, NEW.id, NEW.timestamp_ms)
    ON CONFLICT (host) DO UPDATE SET
        sample_id = excluded.sample_id,
        timestamp_ms = excluded.timestamp_ms
    WHERE excluded.timestamp_ms >= host_latest.timestamp_ms;
END;

CREATE TRIGGER IF NOT EXISTS service_checks_latest AFTER INSERT ON service_checks
BEGIN
    INSERT INTO service_latest (host, service, check_id, timestamp_ms)
    VALUES (NEW.host, NEW.service, NEW.id, NEW.timestamp_ms)
    ON CONFLICT (host, service) DO UPDATE SET
        check_id = excluded.check_id,
        timestamp_ms = excluded.timestamp_ms
    WHERE excluded.timestamp_ms >= service_latest.timestamp_ms;
END;

-- Backfill from the newest rows only (rowid range, no full scan): every live agent reports far
-- more often than this window, and anything it misses fills in on the agent's next batch.
INSERT INTO host_latest (host, sample_id, timestamp_ms)
SELECT host, id, timestamp_ms
FROM host_samples
WHERE id > (SELECT COALESCE(MAX(id), 0) - 5000 FROM host_samples)
ORDER BY timestamp_ms, id
ON CONFLICT (host) DO UPDATE SET
    sample_id = excluded.sample_id,
    timestamp_ms = excluded.timestamp_ms
WHERE excluded.timestamp_ms >= host_latest.timestamp_ms;

INSERT INTO service_latest (host, service, check_id, timestamp_ms)
SELECT host, service, id, timestamp_ms
FROM service_checks
WHERE id > (SELECT COALESCE(MAX(id), 0) - 20000 FROM service_checks)
ORDER BY timestamp_ms, id
ON CONFLICT (host, service) DO UPDATE SET
    check_id = excluded.check_id,
    timestamp_ms = excluded.timestamp_ms
WHERE excluded.timestamp_ms >= service_latest.timestamp_ms;
