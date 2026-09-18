#!/usr/bin/env bash
# Build & push tiny-log-agent Linux images via docker buildx.
#
# Required for push:
#   REGISTRY=ghcr.io/myorg
#
# Optional:
#   VERSION=1.0.0
#   PLATFORMS=linux/amd64,linux/arm64
#   PUSH=1
#   LATEST=1
#   IMAGE_NAME=tiny-log-agent
#   BUILDER=tiny-log-builder

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REGISTRY="${REGISTRY:-}"
VERSION="${VERSION:-}"
PLATFORMS="${PLATFORMS:-linux/amd64,linux/arm64}"
PUSH="${PUSH:-1}"
LATEST="${LATEST:-1}"
BUILDER="${BUILDER:-tiny-log-builder}"
IMAGE_NAME="${IMAGE_NAME:-tiny-log-agent}"

if [[ -z "$VERSION" ]]; then
  VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/agent/Cargo.toml" | head -1)"
fi
if [[ -z "$VERSION" ]]; then
  VERSION="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || date +%Y%m%d%H%M%S)"
fi

if [[ "$PUSH" == "1" && -z "$REGISTRY" ]]; then
  echo "error: set REGISTRY (e.g. REGISTRY=ghcr.io/myorg) when PUSH=1" >&2
  exit 1
fi

if [[ -n "$REGISTRY" ]]; then
  IMAGE="${REGISTRY%/}/${IMAGE_NAME}"
else
  IMAGE="$IMAGE_NAME"
fi

if ! docker buildx inspect "$BUILDER" >/dev/null 2>&1; then
  echo "Creating buildx builder: $BUILDER"
  docker buildx create --name "$BUILDER" --driver docker-container --bootstrap
fi
docker buildx use "$BUILDER"
docker buildx inspect --bootstrap >/dev/null

TAG_ARGS=(-t "${IMAGE}:${VERSION}")
if [[ "$LATEST" == "1" ]]; then
  TAG_ARGS+=(-t "${IMAGE}:latest")
fi

CMD=(
  docker buildx build
  --builder "$BUILDER"
  --platform "$PLATFORMS"
  --pull
  --provenance=false
  -f "$ROOT/agent/Dockerfile"
  "${TAG_ARGS[@]}"
)

if [[ "$PUSH" == "1" ]]; then
  CMD+=(--push)
else
  if [[ "$PLATFORMS" == *","* ]]; then
    echo "error: PUSH=0 requires a single platform (e.g. PLATFORMS=linux/amd64)" >&2
    exit 1
  fi
  CMD+=(--load)
fi

CMD+=("$ROOT/agent")

echo "==> Building tiny-log-agent (${PLATFORMS})"
echo "    ${IMAGE}:${VERSION}$([[ "$LATEST" == "1" ]] && echo " ${IMAGE}:latest")"
"${CMD[@]}"

echo "Done."
if [[ "$PUSH" == "1" ]]; then
  echo "Pushed: ${IMAGE}:${VERSION}"
else
  echo "Loaded: ${IMAGE}:${VERSION}"
fi
