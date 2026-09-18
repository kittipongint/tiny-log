#!/bin/sh
set -eu

# Named volumes are often root-owned on first mount. Fix before dropping privileges.
prepare_data_dir() {
  dir="$1"
  mkdir -p "$dir"
  if [ "$(id -u)" = "0" ]; then
    chown -R tinylog:tinylog "$dir" 2>/dev/null || true
    chmod 755 "$dir" 2>/dev/null || true
  fi
}

DATA_DIR="${TINY_LOG_DATA_DIR:-/data}"
prepare_data_dir "$DATA_DIR"
for key in TINY_LOG_LOGS_DATABASE TINY_LOG_SYSTEM_DATABASE TINY_LOG_METRICS_DATABASE; do
  eval "p=\${$key:-}"
  if [ -n "$p" ]; then
    prepare_data_dir "$(dirname "$p")"
  fi
done

if [ "$(id -u)" = "0" ]; then
  # Prefer non-root. Some security_opt (no-new-privileges) block setgroups — fall back.
  if su-exec tinylog /bin/true 2>/dev/null; then
    exec su-exec tinylog /app/tiny-log "$@"
  fi
  echo "tiny-log: warn: cannot drop privileges (setgroups blocked); running as root" >&2
  exec /app/tiny-log "$@"
fi

exec /app/tiny-log "$@"
