#!/usr/bin/env bash
# Build the server and collector images. Both carry the same binary (subcommands decide the
# role); two names keep the deployment readable and let them diverge later.
# Usage: scripts/build-images.sh 0.2.0 [--push]
set -euo pipefail
version="${1:?usage: $0 <version> [--push]}"
repo="${REPO:-docker.io/yansheng133}"
cd "$(dirname "$0")/../backend"
docker build -t "$repo/audit-loupe-server:$version" -t "$repo/audit-loupe-collector:$version" .
if [[ "${2:-}" == "--push" ]]; then
  docker push "$repo/audit-loupe-server:$version"
  docker push "$repo/audit-loupe-collector:$version"
fi
