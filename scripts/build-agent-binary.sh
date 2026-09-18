#!/usr/bin/env bash
# Build ready-to-run tiny-log-agent binary (glibc) for bare-metal Linux.
# Default: linux/amd64 → dist/tiny-log-agent-linux-amd64/
#
# Env:
#   PLATFORMS=linux/amd64          # or linux/arm64
#   OUT_DIR=dist
#   VERSION=1.0.0

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/agent/Cargo.toml" | head -1)}"
VERSION="${VERSION:-1.0.0}"
PLATFORMS="${PLATFORMS:-linux/amd64}"
OUT_DIR="${OUT_DIR:-$ROOT/dist}"
BUILDER="${BUILDER:-tiny-log-builder}"

if [[ "$PLATFORMS" == *","* ]]; then
  echo "error: set a single platform (e.g. PLATFORMS=linux/amd64)" >&2
  exit 1
fi

arch="${PLATFORMS##*/}"
stage="$OUT_DIR/tiny-log-agent-${VERSION}-linux-${arch}"
rm -rf "$stage"
mkdir -p "$stage"

if ! docker buildx inspect "$BUILDER" >/dev/null 2>&1; then
  docker buildx create --name "$BUILDER" --driver docker-container --bootstrap
fi
docker buildx use "$BUILDER"

echo "==> Building tiny-log-agent binary (${PLATFORMS}, glibc)"
docker buildx build \
  --builder "$BUILDER" \
  --platform "$PLATFORMS" \
  --pull \
  -f "$ROOT/agent/Dockerfile.binary" \
  --target export \
  --output "type=local,dest=$stage" \
  "$ROOT/agent"

chmod +x "$stage/tiny-log-agent"
cp "$ROOT/agent/systemd/tiny-log-agent.service" "$stage/"
cp "$ROOT/agent/systemd/agent.env.example" "$stage/"
cp "$ROOT/agent/systemd/agent.toml.example" "$stage/"
cp "$ROOT/agent/bare-metal.example.toml" "$stage/"
cp "$ROOT/agent/systemd/install.sh" "$stage/"
cp "$ROOT/agent/systemd/uninstall.sh" "$stage/"
chmod +x "$stage/install.sh" "$stage/uninstall.sh"

(
  cd "$OUT_DIR"
  tar -czf "tiny-log-agent-${VERSION}-linux-${arch}.tar.gz" "tiny-log-agent-${VERSION}-linux-${arch}"
)

echo "Done:"
echo "  $stage/tiny-log-agent"
echo "  $OUT_DIR/tiny-log-agent-${VERSION}-linux-${arch}.tar.gz"
file "$stage/tiny-log-agent" || true
