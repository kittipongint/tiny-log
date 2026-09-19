#!/bin/sh
# One-shot install for bare-metal Debian/Ubuntu.
# Usage:
#   sudo ./install.sh
#   sudo TINY_LOG_URL=https://logs.example.com TINY_LOG_API_KEY=xxx ./install.sh
#   sudo ./install.sh https://logs.example.com xxx my-host

set -eu

BIN_SRC="$(CDPATH= cd -- "$(dirname "$0")" && pwd)/tiny-log-agent"
BIN_DST=/usr/local/bin/tiny-log-agent
CONF_DIR=/etc/tiny-log
ENV_FILE="$CONF_DIR/agent.env"
TOML_FILE="$CONF_DIR/agent.toml"
UNIT_SRC="$(CDPATH= cd -- "$(dirname "$0")" && pwd)/tiny-log-agent.service"
UNIT_DST=/etc/systemd/system/tiny-log-agent.service
USER_NAME=tiny-log-agent

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root: sudo $0 ..." >&2
  exit 1
fi

if [ ! -x "$BIN_SRC" ]; then
  echo "missing $BIN_SRC (extract the release tarball first)" >&2
  exit 1
fi

URL="${1:-${TINY_LOG_URL:-}}"
KEY="${2:-${TINY_LOG_API_KEY:-}}"
HOST="${3:-${TINY_LOG_HOST_NAME:-$(hostname -s 2>/dev/null || hostname || echo unknown)}}"

if [ -z "$URL" ]; then
  printf "TINY_LOG_URL (e.g. https://logs.example.com): "
  read -r URL
fi
if [ -z "$KEY" ]; then
  printf "TINY_LOG_API_KEY: "
  read -r KEY
fi
if [ -z "$URL" ] || [ -z "$KEY" ]; then
  echo "URL and API key are required" >&2
  exit 1
fi

if ! id "$USER_NAME" >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin "$USER_NAME"
fi

install -m 755 "$BIN_SRC" "$BIN_DST"
mkdir -p "$CONF_DIR"

cat >"$ENV_FILE" <<EOF
TINY_LOG_URL=$URL
TINY_LOG_API_KEY=$KEY
TINY_LOG_HOST_NAME=$HOST
TINY_LOG_DOCKER=false
RUST_LOG=info
EOF
chmod 600 "$ENV_FILE"
chown root:"$USER_NAME" "$ENV_FILE"

if [ ! -f "$TOML_FILE" ]; then
  cat >"$TOML_FILE" <<'EOF'
system_interval_secs = 60
service_interval_secs = 120
docker = false

[[disk]]
name = "root"
path = "/"
EOF
  chmod 644 "$TOML_FILE"
fi

if [ -f "$UNIT_SRC" ]; then
  install -m 644 "$UNIT_SRC" "$UNIT_DST"
else
  cat >"$UNIT_DST" <<EOF
[Unit]
Description=Tiny Log agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$USER_NAME
Group=$USER_NAME
EnvironmentFile=-$ENV_FILE
Environment=TINY_LOG_AGENT_CONFIG=$TOML_FILE
ExecStart=$BIN_DST
Restart=always
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
EOF
fi

systemctl daemon-reload
systemctl enable --now tiny-log-agent

echo
echo "Installed."
echo "  host:    $HOST"
echo "  url:     $URL"
echo "  config:  $ENV_FILE"
echo "  status:  systemctl status tiny-log-agent"
echo "  logs:    journalctl -u tiny-log-agent -f"
