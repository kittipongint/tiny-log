#!/usr/bin/env bash
# Build (+ optional push) tiny-log for Linux from Apple Silicon via buildx.
#
# Defaults match Docker Hub style used by this project:
#   kittipongint/kintstudio:tiny-log-1.0.0
#
# Env:
#   IMAGE=kittipongint/kintstudio:tiny-log-1.0.0
#   PLATFORMS=linux/amd64,linux/arm64   # amd64 required for typical VPS
#   PUSH=0|1                            # 1 = push multi-arch manifest
#   BUILDER=tiny-log-builder

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/server/Cargo.toml" | head -1)}"
VERSION="${VERSION:-1.0.0}"
IMAGE="${IMAGE:-kittipongint/kintstudio:tiny-log-${VERSION}}"
# PLATFORMS="${PLATFORMS:-linux/amd64,linux/arm64}"
PLATFORMS="${PLATFORMS:-linux/amd64}"
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
  -f "$ROOT/server/Dockerfile"
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

CMD+=("$ROOT/server")

echo "==> Building ${IMAGE}"
echo "    platforms: ${PLATFORMS}  push=${PUSH}"
"${CMD[@]}"

echo "Done: ${IMAGE}"
if [[ "$PUSH" == "1" ]]; then
  echo "Verify: docker buildx imagetools inspect ${IMAGE}"
fi
