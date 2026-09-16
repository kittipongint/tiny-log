#!/usr/bin/env bash

set -euo pipefail

IMAGE_NAME="${IMAGE_NAME:-tiny-log}"
AGENT_IMAGE_NAME="${AGENT_IMAGE_NAME:-tiny-log-agent}"
VERSION="${VERSION:-$(git rev-parse --short HEAD 2>/dev/null || date +%Y%m%d%H%M%S)}"

echo "Building ${IMAGE_NAME}:${VERSION}"
docker build \
    --pull \
    -t "${IMAGE_NAME}:${VERSION}" \
    -t "${IMAGE_NAME}:latest" \
    .

echo "Building ${AGENT_IMAGE_NAME}:${VERSION}"
docker build \
    --pull \
    -f agent/Dockerfile \
    -t "${AGENT_IMAGE_NAME}:${VERSION}" \
    -t "${AGENT_IMAGE_NAME}:latest" \
    .

echo
echo "Build completed:"
echo "  ${IMAGE_NAME}:${VERSION}"
echo "  ${IMAGE_NAME}:latest"
echo "  ${AGENT_IMAGE_NAME}:${VERSION}"
echo "  ${AGENT_IMAGE_NAME}:latest"
