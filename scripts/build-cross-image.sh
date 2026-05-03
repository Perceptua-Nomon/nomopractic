#!/usr/bin/env bash
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE_NAME="nomopractic-cross-aarch64"

echo "Building cross Docker image: ${IMAGE_NAME}"
docker build -t "${IMAGE_NAME}" "${DIR}/cross-image"
echo "Built image ${IMAGE_NAME}. To use with scripts/deploy.sh, set CROSS_CONTAINER=${IMAGE_NAME}"