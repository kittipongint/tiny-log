# Tiny Log

Lightweight centralized logging server built with:

- Rust
- Axum
- SQLite
- SQLx
- Vanilla JavaScript
- SSE

The application is intentionally **logging-focused**, not an APM platform.

---

# 1. Product Definition

Tiny Log provides:

```text
receive logs
    ↓
store in SQLite
    ↓
search/filter logs
    ↓
view logs in web UI
    ↓
stream new logs live
```

It must support:

```text
Docker applications
Go applications
Nginx
remote applications
remote servers
browser JavaScript
client-side applications
```

Deployment target:

```text
1 binary
1 container
1 SQLite file
1 admin account
1 web UI
```

No external database is required.

---

# 2. Explicit Non-Goals

Do NOT implement:

```text
APM
distributed tracing
metrics
profiling
Prometheus
Grafana
Loki
Elasticsearch
ClickHouse
Redis
Kafka
PostgreSQL
MySQL
OAuth
OIDC
JWT
multi-user RBAC
email/password reset
frontend framework
Node.js runtime
```

The product should remain small.

---

# 3. Technology Stack

Recommended:

```toml
axum = "0.8"
tokio = { version = "1", features = ["full"] }

serde = { version = "1", features = ["derive"] }
serde_json = "1"

sqlx = {
    version = "0.9",
    features = [
        "runtime-tokio-rustls",
        "sqlite",
        "migrate"
    ]
}

argon2 = "0.6"

tower-http = {
    version = "0.6",
    features = [
        "trace",
        "cors",
        "limit"
    ]
}

rand = "0.9"

chrono = {
    version = "0.4",
    features = ["serde"]
}

tracing = "0.1"

tracing-subscriber = {
    version = "0.3",
    features = ["env-filter"]
}
```

SQLx's SQLite feature uses a bundled SQLite build by default, which makes deployment simpler and avoids depending on the host's SQLite installation.

Argon2 0.6 provides Argon2id support directly.

---

# 4. Project Structure

```text
tiny-log/
├── Cargo.toml
├── Cargo.lock
│
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── state.rs
│   ├── error.rs
│   │
│   ├── auth/
│   │   ├── mod.rs
│   │   ├── password.rs
│   │   ├── session.rs
│   │   └── middleware.rs
│   │
│   ├── models/
│   │   ├── mod.rs
│   │   ├── log.rs
│   │   ├── admin.rs
│   │   ├── session.rs
│   │   └── settings.rs
│   │
│   ├── db/
│   │   ├── mod.rs
│   │   ├── logs.rs
│   │   ├── admin.rs
│   │   ├── sessions.rs
│   │   └── settings.rs
│   │
│   ├── api/
│   │   ├── mod.rs
│   │   ├── auth.rs
│   │   ├── logs.rs
│   │   ├── client_logs.rs
│   │   ├── admin.rs
│   │   ├── stream.rs
│   │   └── health.rs
│   │
│   ├── cli/
│   │   ├── mod.rs
│   │   └── admin.rs
│   │
│   ├── retention/
│   │   └── mod.rs
│   │
│   └── web/
│       └── mod.rs
│
├── migrations/
│   └── 0001_initial.sql
│
├── web/
│   ├── login.html
│   ├── index.html
│   ├── settings.html
│   ├── app.js
│   ├── login.js
│   ├── settings.js
│   └── style.css
│
├── scripts/
│   └── build.sh
│
├── Dockerfile
├── docker-compose.dev.yml
├── docker-compose.prod.yml
├── .dockerignore
└── .gitignore
```

---

# 5. Application Commands

The same binary supports commands:

```text
tiny-log serve
tiny-log migrate

tiny-log admin create
tiny-log admin passwd
tiny-log admin info
```

## Serve

```bash
tiny-log serve
```

Starts HTTP server.

## Migrate

```bash
tiny-log migrate
```

Creates or updates SQLite schema.

## Admin Create

```bash
tiny-log admin create
```

Interactively asks:

```text
Username:
Password:
Confirm password:
```

## Admin Password

```bash
tiny-log admin passwd
```

Replaces password after verification.

## Admin Info

```bash
tiny-log admin info
```

Outputs:

```text
Username: admin
Created: ...
Updated: ...
```

Never output password or password hash.

---

# 6. Configuration

Environment variables:

```env
TINY_LOG_HOST=0.0.0.0
TINY_LOG_PORT=8080

TINY_LOG_DATABASE=/data/logs.db

TINY_LOG_API_KEY=
TINY_LOG_CLIENT_TOKEN=

TINY_LOG_REQUIRE_AUTH=true

TINY_LOG_RETENTION_DAYS=30
TINY_LOG_SESSION_DAYS=7

TINY_LOG_COOKIE_SECURE=true

TINY_LOG_MAX_BODY_MB=1
TINY_LOG_MAX_BATCH=500

RUST_LOG=info
```

There is deliberately NO:

```text
TINY_LOG_ADMIN_PASSWORD
TINY_LOG_ADMIN_USERNAME
```

Admin credentials live in SQLite.

---

# 7. SQLite Database

Database file:

```text
/data/logs.db
```

SQLite configuration:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;
PRAGMA foreign_keys = ON;
PRAGMA auto_vacuum = INCREMENTAL;
```

Use SQLx SQLite connection pool.

Recommended:

```text
max_connections = 5
min_connections = 1
```

SQLx uses background worker threads for SQLite access, allowing its async interface without blocking the main async execution path.

---

# 8. Database Schema

```sql
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
```

---

# 9. Log Data Model

Required:

```text
app
level
message
```

Optional:

```text
timestamp
source
meta
```

Example:

```json
{
  "app": "wordyguru",
  "level": "error",
  "message": "MySQL timeout",
  "timestamp": "2026-09-15T15:20:31.123Z",
  "source": "server",
  "meta": {
    "route": "/search",
    "status": 504,
    "duration_ms": 5000
  }
}
```

If timestamp is omitted:

```text
server current UTC time
```

---

# 10. Log Levels

Supported:

```text
debug
info
warn
error
fatal
```

Input should be normalized to lowercase.

Unknown level:

```text
400 Bad Request
```

---

# 11. API Endpoints

## Health

```http
GET /health
```

Response:

```json
{
  "status": "ok"
}
```

No authentication.

---

## Login

```http
POST /api/auth/login
```

Request:

```json
{
  "username": "admin",
  "password": "..."
}
```

Successful:

```text
200 OK
```

Sets:

```text
tiny_log_session
```

Invalid:

```text
401 Unauthorized
```

---

## Logout

```http
POST /api/auth/logout
```

Requires authenticated session.

---

## Current User

```http
GET /api/auth/me
```

Response:

```json
{
  "authenticated": true,
  "username": "admin",
  "role": "admin"
}
```

---

# 12. Log Write API

## Single

```http
POST /api/v1/logs
```

Authentication:

```text
Authorization: Bearer API_KEY
```

Response:

```json
{
  "success": true,
  "id": 12345
}
```

---

## Batch

```http
POST /api/v1/logs/batch
```

Maximum:

```text
500 records
```

Example:

```json
{
  "logs": [
    {
      "app": "wordyguru",
      "level": "info",
      "message": "request completed"
    },
    {
      "app": "nginx",
      "level": "error",
      "message": "upstream timeout"
    }
  ]
}
```

One transaction per batch.

Either:

```text
all inserted
```

or:

```text
all rolled back
```

---

# 13. Browser Client Log API

Dedicated endpoint:

```http
POST /api/v1/client/logs
```

Authentication:

```text
Authorization: Bearer CLIENT_TOKEN
```

This token can:

```text
WRITE logs
```

but cannot:

```text
READ logs
CHANGE settings
LOGIN
DELETE logs
```

Example:

```javascript
function tinyLog(level, message, meta = {}) {
    fetch("https://logs.example.com/api/v1/client/logs", {
        method: "POST",
        headers: {
            "Content-Type": "application/json",
            "Authorization": "Bearer YOUR_CLIENT_TOKEN"
        },
        body: JSON.stringify({
            app: "wordyguru-web",
            level,
            message,
            source: "browser",
            meta
        }),
        keepalive: true
    });
}
```

---

# 14. Log Read API

```http
GET /api/v1/logs
```

Requires admin session.

Supported parameters:

```text
app
level
source
search
from
to
limit
offset
```

Example:

```text
/api/v1/logs?app=wordyguru&level=error&limit=100
```

Default:

```text
limit = 100
```

Maximum:

```text
500
```

Sort:

```text
timestamp_ms DESC
id DESC
```

---

# 15. Log Search

Initial search:

```sql
WHERE message LIKE ?
```

Do not add SQLite FTS5 in v1.

Future version may introduce FTS5 if needed.

---

# 16. Log Detail

```http
GET /api/v1/logs/:id
```

Admin only.

Response:

```json
{
  "id": 12345,
  "timestamp": "2026-09-15T15:20:31.123Z",
  "app": "wordyguru",
  "level": "error",
  "source": "server",
  "message": "MySQL timeout",
  "meta": {
    "route": "/search"
  }
}
```

---

# 17. Live Log Stream

Use Server-Sent Events.

```http
GET /api/v1/logs/stream
```

Admin only.

Implementation:

```text
SQLite successful INSERT
          │
          ▼
broadcast channel
          │
          ├── browser 1
          ├── browser 2
          └── browser 3
```

Broadcast only after successful DB commit.

SQLite remains the source of truth.

---

# 18. Admin Authentication

Exactly one admin account.

Schema:

```sql
CREATE TABLE IF NOT EXISTS admin_user (
    id INTEGER PRIMARY KEY CHECK (id = 1),

    username TEXT NOT NULL UNIQUE,

    password_hash TEXT NOT NULL,

    created_at INTEGER NOT NULL,

    updated_at INTEGER NOT NULL
);
```

---

# 19. Password Hashing

Use:

```text
Argon2id
```

Never use:

```text
MD5
SHA1
SHA256
SHA512
```

as the password storage algorithm.

Recommended parameters:

```text
Algorithm:   Argon2id
Version:     19
Memory:      64 MiB
Iterations:  3
Parallelism: 2
Salt:        16+ bytes
Output:      32 bytes
```

Store standard PHC encoded output:

```text
$argon2id$...
```

The `argon2` Rust crate supports Argon2id directly.

---

# 20. Admin Creation

First initialize database:

```bash
docker compose run --rm tiny-log migrate
```

Then:

```bash
docker compose run --rm -it tiny-log admin create
```

Interactive:

```text
Username: admin
Password:
Confirm password:
```

The password is:

```text
entered
→ hashed with Argon2id
→ inserted into SQLite
```

Plaintext is never written to:

```text
database
environment
docker metadata
logs
config file
```

---

# 21. Password Change

```bash
tiny-log admin passwd
```

or Web UI:

```text
Settings
→ Change password
```

Require:

```text
current password
new password
confirm password
```

Minimum:

```text
12 characters
```

After password change:

```sql
DELETE FROM sessions;
```

All existing sessions become invalid.

---

# 22. Session Authentication

Use server-side SQLite sessions.

Cookie:

```text
tiny_log_session
```

Attributes:

```text
HttpOnly
Secure=true in production
SameSite=Lax
Path=/
```

Session default lifetime:

```text
7 days
```

Config:

```text
TINY_LOG_SESSION_DAYS=7
```

Session token must be generated using cryptographically secure randomness.

---

# 23. Login Rate Limiting

Protect the login endpoint.

Initial rule:

```text
5 failed attempts / 5 minutes / IP
```

Then:

```text
429 Too Many Requests
```

An in-memory limiter is sufficient for v1.

No Redis.

---

# 24. Web UI

Unauthenticated:

```text
/
```

must display login page.

After authentication:

```text
/
```

shows logs.

Navigation:

```text
Logs
Settings
admin
Logout
```

---

# 25. Login UI

```text
┌──────────────────────────────┐
│          Tiny Log            │
│                              │
│ Username                     │
│ [________________________]   │
│                              │
│ Password                     │
│ [________________________]   │
│                              │
│          [ Login ]           │
└──────────────────────────────┘
```

No registration.

No forgot-password flow.

---

# 26. Log UI

```text
┌─────────────────────────────────────────────────────┐
│ Tiny Log                             admin  Logout   │
├─────────────────────────────────────────────────────┤
│ App      [All ▼]                                    │
│ Level    [All ▼]                                    │
│ Search   [________________________] [Search]        │
│                                                     │
│ [Live ●]                                            │
├─────────────────────────────────────────────────────┤
│ Time       App         Level     Message             │
│ 22:41:03   wordyguru   ERROR     MySQL timeout       │
│ 22:41:02   nginx       INFO      GET /article/foo    │
│ 22:40:59   worker      WARN      retry job           │
└─────────────────────────────────────────────────────┘
```

Click row:

```text
timestamp
app
level
source
message
metadata
```

---

# 27. Live Mode

Button:

```text
Live
```

uses:

```javascript
new EventSource("/api/v1/logs/stream")
```

New logs appear automatically.

If user scrolls away from newest logs:

```text
12 new logs
```

must appear.

Do not forcibly scroll the page while user is reading older logs.

---

# 28. Settings UI

```text
Settings

Log Retention
[ 10 ] days

Session Lifetime
[ 7 ] days

[ Save ]
```

Security:

```text
Change Password
```

Retention:

```text
[ Clean up old logs now ]
```

---

# 29. Retention

Default:

```text
30 days
```

Example:

```text
TINY_LOG_RETENTION_DAYS=10
```

means:

```text
keep approximately 10 days of logs
delete older logs automatically
```

Allowed:

```text
1 - 3650 days
```

---

# 30. Runtime Retention Configuration

Environment variable provides the initial value:

```text
TINY_LOG_RETENTION_DAYS=10
```

On first initialization:

```text
environment
    ↓
SQLite settings
```

After SQLite initialization:

```text
SQLite settings = source of truth
```

Changing retention in Web UI persists across restart.

Example:

```text
initial = 30
admin changes = 10
restart
→ still 10
```

---

# 31. Settings Table

```sql
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
```

Initial keys:

```text
retention_days
session_days
```

---

# 32. Retention API

Get settings:

```http
GET /api/v1/admin/settings
```

Update:

```http
PUT /api/v1/admin/settings
```

Example:

```json
{
    "retention_days": 10
}
```

Response:

```json
{
    "success": true,
    "retention_days": 10
}
```

---

# 33. Automatic Cleanup

Background Tokio task:

```text
every 1 hour
```

Run:

```sql
DELETE FROM logs
WHERE timestamp_ms < ?;
```

Then:

```sql
PRAGMA wal_checkpoint(TRUNCATE);
PRAGMA incremental_vacuum;
```

Do not run full `VACUUM` after every cleanup.

---

# 34. Manual Cleanup

Admin-only:

```http
POST /api/v1/admin/retention/run
```

Uses current:

```text
retention_days
```

Response:

```json
{
    "deleted": 182341
}
```

UI confirmation required:

```text
Delete all logs older than 10 days?

[Cancel] [Delete]
```

---

# 35. Payload Limits

Defaults:

```text
max single request = 256 KB
max batch request = 1 MB
max batch records = 500
max message = 64 KB
max metadata = 128 KB
```

If exceeded:

```text
413 Payload Too Large
```

---

# 36. Security

Required:

- Argon2id password hashing
- Secure random salts
- Secure random session tokens
- HttpOnly cookies
- Secure cookies in production
- SameSite=Lax
- login rate limiting
- request body limits
- batch size limits
- admin-only log reads
- write-only client token
- output escaping in browser
- generic login errors
- no password logging
- no password hash exposure
- non-root Docker user

---

# 37. CORS

Default:

```text
disabled / same-origin
```

Optional environment variable:

```text
TINY_LOG_CORS_ORIGIN=https://example.com
```

Never default to unrestricted:

```text
*
```

for production.

---

# 38. API Authorization Model

## Admin session

Can:

```text
READ logs
READ log details
READ stream
CHANGE settings
CHANGE password
RUN retention
```

## Server API key

Can:

```text
WRITE logs
WRITE batch logs
```

Cannot:

```text
READ logs
CHANGE settings
LOGIN
```

## Browser client token

Can:

```text
WRITE browser logs
```

Cannot:

```text
READ logs
CHANGE settings
LOGIN
```

---

# 39. HTTP Routes

```text
GET  /health

GET  /login
GET  /
GET  /settings

POST /api/auth/login
POST /api/auth/logout
GET  /api/auth/me

POST /api/v1/logs
POST /api/v1/logs/batch
POST /api/v1/client/logs

GET  /api/v1/logs
GET  /api/v1/logs/:id
GET  /api/v1/logs/stream

GET  /api/v1/admin/settings
PUT  /api/v1/admin/settings
POST /api/v1/admin/retention/run
POST /api/v1/admin/password

GET  /api/v1/info
```

---

# 40. Docker Image

Use multi-stage build.

Builder:

```dockerfile
FROM rust:1.98-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY web ./web

RUN cargo build --release
```

Runtime:

```dockerfile
FROM debian:bookworm-slim

RUN useradd \
    --system \
    --uid 10001 \
    --create-home \
    tinylog

WORKDIR /app

COPY --from=builder \
    /app/target/release/tiny-log \
    /app/tiny-log

COPY web /app/web

RUN mkdir -p /data \
    && chown -R tinylog:tinylog /data /app

USER tinylog

ENV TINY_LOG_HOST=0.0.0.0
ENV TINY_LOG_PORT=8080
ENV TINY_LOG_DATABASE=/data/logs.db

EXPOSE 8080

VOLUME ["/data"]

ENTRYPOINT ["/app/tiny-log"]
```

Application listens on:

```text
0.0.0.0:TINY_LOG_PORT
```

---

# 41. Custom Port

Default:

```text
8080
```

Custom:

```bash
TINY_LOG_PORT=9000 tiny-log serve
```

Docker:

```bash
docker run \
  -p 9000:9000 \
  -e TINY_LOG_PORT=9000 \
  tiny-log:latest
```

The application must not assume port 8080 internally.

---

# 42. Development Docker Compose

`docker-compose.dev.yml`:

```yaml
services:
  tiny-log:
    image: rust:1.98-bookworm

    working_dir: /app

    command:
      - cargo
      - run
      - --bin
      - tiny-log
      - --
      - serve

    ports:
      - "${TINY_LOG_PORT:-8080}:${TINY_LOG_PORT:-8080}"

    environment:
      TINY_LOG_HOST: 0.0.0.0
      TINY_LOG_PORT: ${TINY_LOG_PORT:-8080}
      TINY_LOG_DATABASE: /data/logs.db

      TINY_LOG_API_KEY: dev-api-key
      TINY_LOG_CLIENT_TOKEN: dev-client-token

      TINY_LOG_REQUIRE_AUTH: "true"

      TINY_LOG_RETENTION_DAYS: 10
      TINY_LOG_SESSION_DAYS: 7

      TINY_LOG_COOKIE_SECURE: "false"

      RUST_LOG: debug

    volumes:
      - ./:/app
      - ./data:/data
      - cargo-cache:/usr/local/cargo/registry
      - target-cache:/app/target

volumes:
  cargo-cache:
  target-cache:
```

---

# 43. Development Setup

```bash
mkdir -p data

docker compose \
  -f docker-compose.dev.yml \
  run --rm \
  tiny-log migrate
```

Create admin:

```bash
docker compose \
  -f docker-compose.dev.yml \
  run --rm -it \
  tiny-log admin create
```

Start:

```bash
docker compose \
  -f docker-compose.dev.yml \
  up
```

Open:

```text
http://localhost:8080
```

---

# 44. Production Compose

`docker-compose.prod.yml`:

```yaml
services:
  tiny-log:
    image: tiny-log:latest

    container_name: tiny-log

    restart: unless-stopped

    ports:
      - "${TINY_LOG_PORT:-8080}:${TINY_LOG_PORT:-8080}"

    environment:
      TINY_LOG_HOST: 0.0.0.0
      TINY_LOG_PORT: ${TINY_LOG_PORT:-8080}
      TINY_LOG_DATABASE: /data/logs.db

      TINY_LOG_API_KEY: ${TINY_LOG_API_KEY}
      TINY_LOG_CLIENT_TOKEN: ${TINY_LOG_CLIENT_TOKEN}

      TINY_LOG_REQUIRE_AUTH: ${TINY_LOG_REQUIRE_AUTH:-true}

      TINY_LOG_RETENTION_DAYS: ${TINY_LOG_RETENTION_DAYS:-10}
      TINY_LOG_SESSION_DAYS: ${TINY_LOG_SESSION_DAYS:-7}

      TINY_LOG_COOKIE_SECURE: ${TINY_LOG_COOKIE_SECURE:-true}

      RUST_LOG: ${RUST_LOG:-info}

    volumes:
      - tiny-log-data:/data

    healthcheck:
      test:
        [
          "CMD",
          "wget",
          "-q",
          "-O",
          "-",
          "http://127.0.0.1:${TINY_LOG_PORT:-8080}/health"
        ]
      interval: 30s
      timeout: 5s
      retries: 3

volumes:
  tiny-log-data:
```

---

# 45. Production Initialization

Create volume:

```bash
docker volume create tiny-log-data
```

Initialize database:

```bash
docker compose \
  -f docker-compose.prod.yml \
  run --rm tiny-log migrate
```

Create admin:

```bash
docker compose \
  -f docker-compose.prod.yml \
  run --rm -it tiny-log admin create
```

Start:

```bash
docker compose \
  -f docker-compose.prod.yml \
  up -d
```

---

# 46. Build Script

`scripts/build.sh`:

```bash
#!/usr/bin/env bash

set -euo pipefail

IMAGE_NAME="${IMAGE_NAME:-tiny-log}"
VERSION="${VERSION:-$(git rev-parse --short HEAD 2>/dev/null || date +%Y%m%d%H%M%S)}"

echo "Building ${IMAGE_NAME}:${VERSION}"

docker build \
    --pull \
    -t "${IMAGE_NAME}:${VERSION}" \
    -t "${IMAGE_NAME}:latest" \
    .

echo
echo "Build completed:"
echo "  ${IMAGE_NAME}:${VERSION}"
echo "  ${IMAGE_NAME}:latest"
```

Make executable:

```bash
chmod +x scripts/build.sh
```

Build:

```bash
./scripts/build.sh
```

Custom:

```bash
IMAGE_NAME=myregistry/tiny-log \
VERSION=1.0.0 \
./scripts/build.sh
```

---

# 47. Nginx Deployment

Recommended:

```text
Internet
   │
   ▼
Nginx / HTTPS
   │
   ▼
Tiny Log :8080
```

Example:

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;

    proxy_http_version 1.1;

    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;

    proxy_buffering off;
}
```

`proxy_buffering off` is required for reliable SSE delivery.

---

# 48. Browser Security

All log content is untrusted.

Never render:

```javascript
element.innerHTML = log.message;
```

Use:

```javascript
element.textContent = log.message;
```

Metadata should be rendered as escaped/preformatted JSON.

This prevents stored XSS from malicious log payloads.

---

# 49. Backup

SQLite database:

```text
/data/logs.db
```

Recommended backup:

```bash
sqlite3 /data/logs.db \
  ".backup '/backup/logs.db'"
```

Do not rely on a simple file copy while SQLite is actively writing.

Backup frequency:

```text
daily
```

for a small deployment.

---

# 50. Tiny Log Internal Logging

Tiny Log itself writes operational logs to stdout.

Example:

```text
INFO server_started addr=0.0.0.0:8080
INFO database_opened path=/data/logs.db
INFO retention_started days=10
WARN unauthorized_request path=/api/v1/logs
ERROR database_error ...
```

Do not insert Tiny Log's own internal logs back into the Tiny Log database.

Avoid recursive logging.

---

# 51. Performance Targets

Target:

```text
idle RAM < 30 MB
normal runtime < 64 MB
```

These are engineering targets, not hard guarantees.

Focus optimization on:

```text
SQLite WAL
batch inserts
small connection pool
small number of indexes
prepared SQL
bounded request size
```

Target a basic workload of approximately:

```text
1,000+ logs/sec
```

on a normal small VPS, but benchmark before treating this as a production capacity guarantee.

---

# 52. Expected Small Server Deployment

Example:

```text
1 vCPU
512 MB RAM
20 GB SSD
10-day retention
```

Architecture:

```text
                  Tiny Log
             ┌─────────────────┐
             │ Rust / Axum     │
             │ SQLite          │
             │ Web UI          │
             │ SSE             │
             └────────┬────────┘
                      │
                 /data/logs.db
```

No other service required.

---

# 53. Typical Log Sources

### Go

```go
tinyLog("info", "request completed", meta)
```

### Nginx

Forward parsed access/error records.

### Docker

Applications send logs to Tiny Log via the API.

### Remote server

```bash
curl https://logs.example.com/api/v1/logs
```

### Browser

```javascript
tinyLog(
    "error",
    "fetch failed",
    { endpoint: "/api/search" }
);
```

---

# 54. Acceptance Tests

## Authentication

- Login works.
- Wrong username/password returns 401.
- Password is stored only as Argon2id hash.
- Password is never logged.
- Session cookie is HttpOnly.
- Logout invalidates session.
- Password change invalidates all sessions.
- Only one admin record exists.

## Logs

- Single insert works.
- Batch insert works.
- Invalid payload returns 400.
- Oversized payload returns 413.
- Admin can search logs.
- Admin can filter by app.
- Admin can filter by level.
- Admin can inspect metadata.
- Live stream works.

## Retention

- `retention_days=10` works.
- Admin can change retention from UI.
- Setting survives restart.
- Old logs are automatically removed.
- Manual cleanup works.

## Docker

- Build succeeds.
- Container runs as non-root.
- Custom port works.
- SQLite volume persists across container replacement.
- Health check works.

## Browser

- Login page works.
- Logs page works.
- Live updates work.
- XSS payloads are rendered as text.
- Browser client token cannot read logs.

---

# 55. First Implementation Priority

Implement in this order:

```text
1. Rust project / configuration
2. SQLite connection + migration
3. Log model + INSERT
4. POST /api/v1/logs
5. POST /api/v1/logs/batch
6. GET /api/v1/logs
7. Basic Web UI
8. Admin Argon2id authentication
9. SQLite sessions
10. SSE live logs
11. Retention worker
12. Settings UI
13. Browser client endpoint
14. Docker production image
15. Dev/prod Compose
16. Build script
17. Security hardening
```

Do not implement advanced features before these are complete.

---

# 56. Final Architecture

```text
                         ┌─────────────────┐
Docker Apps ────────────►│                 │
Go Apps ────────────────►│                 │
Nginx ──────────────────►│    Tiny Log     │
Remote Servers ─────────►│   Rust/Axum     │
Browser JS ─────────────►│                 │
                         └────────┬────────┘
                                  │
                    ┌─────────────┼─────────────┐
                    │             │             │
                    ▼             ▼             ▼
                  Logs         Sessions      Settings
                    │             │             │
                    └─────────────┼─────────────┘
                                  ▼
                               SQLite
                              logs.db
                                  │
                                  ▼
                              Admin UI
```

Final principle:

> **Tiny Log is a small log server, not an observability platform.**

The product should remain useful because it is simple:

```text
Rust
+
SQLite
+
REST
+
SSE
+
Vanilla JS
+
one admin
```

No additional infrastructure should be required.