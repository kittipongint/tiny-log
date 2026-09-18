#!/usr/bin/env bash
# Build (+ optional push) tiny-log-agent for Linux from Apple Silicon via buildx.
#
# Defaults:
#   kittipongint/kintstudio:tiny-log-agent-1.0.0
#
# Env:
#   IMAGE=kittipongint/kintstudio:tiny-log-agent-1.0.0
#   PLATFORMS=linux/amd64,linux/arm64
#   PUSH=0|1
#   BUILDER=tiny-log-builder

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/agent/Cargo.toml" | head -1)}"
VERSION="${VERSION:-1.0.0}"
IMAGE="${IMAGE:-kittipongint/kintstudio:tiny-log-agent-${VERSION}}"
# PLATFORMS="${PLATFORMS:-linux/amd64,linux/arm64}"
PLATFORMS="linux/amd64"
PUSH="${PUSH:-0}"
BUILDER="${BUILDER:-tiny-log-builder}"

if ! docker buildx inspect "$BUILDER" >/dev/null 2>&1; then
  echo "Creating buildx builder: $BUILDER"
  docker buildx create --name "$BUILDER" --driver docker-container --bootstrap
fi
docker buildx use "$BUILDER"
docker buildx inspect --bootstrap >/dev/null

CMD=(
  docker buildx build
  --builder "$BUILDER"
  --platform "$PLATFORMS"
  --pull
  --provenance=false
  -f "$ROOT/agent/Dockerfile"
  -t "$IMAGE"
)

if [[ "$PUSH" == "1" ]]; then
  CMD+=(--push)
else
  if [[ "$PLATFORMS" == *","* ]]; then
    echo "error: multi-platform build needs PUSH=1 (or set PLATFORMS=linux/amd64)" >&2
    exit 1
  fi
  CMD+=(--load)
fi

CMD+=("$ROOT/agent")

echo "==> Building ${IMAGE}"
echo "    platforms: ${PLATFORMS}  push=${PUSH}"
"${CMD[@]}"

echo "Done: ${IMAGE}"
if [[ "$PUSH" == "1" ]]; then
  echo "Verify: docker buildx imagetools inspect ${IMAGE}"
fi
