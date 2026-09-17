ALTER TABLE service_checks ADD COLUMN cpu_pct REAL;
ALTER TABLE service_checks ADD COLUMN mem_used_bytes INTEGER;
ALTER TABLE service_checks ADD COLUMN mem_limit_bytes INTEGER;
ALTER TABLE service_checks ADD COLUMN load_hint TEXT;
ALTER TABLE service_checks ADD COLUMN recommend TEXT;
