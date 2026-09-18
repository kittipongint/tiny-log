#!/bin/sh
# Uninstall bare-metal tiny-log-agent installed by install.sh
# Usage: sudo ./uninstall.sh

set -eu

BIN_DST=/usr/local/bin/tiny-log-agent
CONF_DIR=/etc/tiny-log
UNIT_DST=/etc/systemd/system/tiny-log-agent.service
USER_NAME=tiny-log-agent

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root: sudo $0" >&2
  exit 1
fi

if command -v systemctl >/dev/null 2>&1; then
  systemctl stop tiny-log-agent 2>/dev/null || true
  systemctl disable tiny-log-agent 2>/dev/null || true
fi

rm -f "$UNIT_DST"
rm -f "$BIN_DST"

if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload 2>/dev/null || true
  systemctl reset-failed tiny-log-agent 2>/dev/null || true
fi

REMOVE_CONF=0
REMOVE_USER=0
if [ "${1:-}" = "--purge" ] || [ "${1:-}" = "-p" ]; then
  REMOVE_CONF=1
  REMOVE_USER=1
else
  printf "Remove config %s ? [y/N] " "$CONF_DIR"
  read -r ans || ans=
  case "$ans" in y|Y|yes|YES) REMOVE_CONF=1 ;; esac
  printf "Remove user %s ? [y/N] " "$USER_NAME"
  read -r ans || ans=
  case "$ans" in y|Y|yes|YES) REMOVE_USER=1 ;; esac
fi

if [ "$REMOVE_CONF" -eq 1 ]; then
  rm -rf "$CONF_DIR"
  echo "removed $CONF_DIR"
else
  echo "kept $CONF_DIR"
fi

if [ "$REMOVE_USER" -eq 1 ]; then
  if id "$USER_NAME" >/dev/null 2>&1; then
    userdel "$USER_NAME" 2>/dev/null || true
    echo "removed user $USER_NAME"
  fi
else
  echo "kept user $USER_NAME"
fi

echo "Uninstalled tiny-log-agent."
echo "  full purge: sudo $0 --purge"
