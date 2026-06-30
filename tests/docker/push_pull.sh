#!/usr/bin/env bash
# Portable `docker login` + push + pull round-trip against a running ruskery.
# Used by the docker-e2e workflow on Linux, macOS, and (via Git Bash) Windows.
#
# Required env:
#   REGISTRY   host:port of the registry (e.g. localhost:5000)
#   RUSER      username
#   TOKEN      personal access token (used as the docker password)
#   SRC        source image to round-trip (e.g. busybox:latest)
#   DEST       destination repo:tag within the registry (e.g. acme/app:ci)
set -euo pipefail

: "${REGISTRY:?}"; : "${RUSER:?}"; : "${TOKEN:?}"; : "${SRC:?}"; : "${DEST:?}"

echo "==> docker login $REGISTRY as $RUSER"
echo "$TOKEN" | docker login "$REGISTRY" -u "$RUSER" --password-stdin

echo "==> pull source image: $SRC"
docker pull "$SRC"

echo "==> tag + push to ruskery: $REGISTRY/$DEST"
docker tag "$SRC" "$REGISTRY/$DEST"
docker push "$REGISTRY/$DEST"

# Capture the digest the registry reported, then drop local copies so the next
# pull is a real network fetch (blob bytes come back via the presigned redirect).
pushed_digest="$(docker inspect --format '{{index .RepoDigests 0}}' "$REGISTRY/$DEST" || true)"
echo "==> pushed digest: ${pushed_digest:-<none>}"

docker rmi "$REGISTRY/$DEST" "$SRC" >/dev/null 2>&1 || true

echo "==> pull back from ruskery"
docker pull "$REGISTRY/$DEST"

echo "==> verify it runs"
docker image inspect "$REGISTRY/$DEST" >/dev/null

echo "OK: $REGISTRY/$DEST pushed and pulled successfully"
