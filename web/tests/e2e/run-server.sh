#!/usr/bin/env bash
# Start a ruskery instance for the Playwright dashboard e2e: fresh temp DB, TLS
# off, fast analytics rollup, lazy/dummy storage (most UI flows need no S3).
#
# In CI set RUSKERY_BIN to a prebuilt (release) binary — it embeds the UI. Locally
# this builds the debug binary, which serves web/.output/public from disk, so the
# UI is regenerated first.
set -euo pipefail

WEB="$(cd "$(dirname "$0")/../.." && pwd)"   # web/
ROOT="$(cd "$WEB/.." && pwd)"                # repo root
PORT="${E2E_PORT:-8099}"
DB="$WEB/tests/e2e/.tmp/e2e.db"

mkdir -p "$(dirname "$DB")"
rm -f "$DB" "$DB-shm" "$DB-wal"

export RUSKERY_SERVER__HTTP_ADDR="127.0.0.1:${PORT}"
export RUSKERY_SERVER__PUBLIC_URL="http://127.0.0.1:${PORT}"
export RUSKERY_TLS__ENABLED=false
export RUSKERY_ANALYTICS__ROLLUP_SECS=1
export RUSKERY_DATABASE__PATH="$DB"
export RUSKERY_STORAGE__ENDPOINT="${RUSKERY_STORAGE__ENDPOINT:-http://127.0.0.1:59000}"
export RUSKERY_STORAGE__BUCKET="${RUSKERY_STORAGE__BUCKET:-ruskery}"
export RUSKERY_STORAGE__REGION="${RUSKERY_STORAGE__REGION:-us-east-1}"
export RUSKERY_STORAGE__ACCESS_KEY_ID="${RUSKERY_STORAGE__ACCESS_KEY_ID:-test}"
export RUSKERY_STORAGE__SECRET_ACCESS_KEY="${RUSKERY_STORAGE__SECRET_ACCESS_KEY:-test}"
export RUSKERY_STORAGE__FORCE_PATH_STYLE=true
export RUST_LOG="${RUST_LOG:-warn}"

# CI sets RUSKERY_BIN to a prebuilt (release) binary that embeds the UI. Locally,
# regenerate the on-disk UI (the debug build reads it from disk) and let cargo
# build + locate the binary — no hardcoded target path, so it stays portable.
if [ -n "${RUSKERY_BIN:-}" ]; then
  exec "$RUSKERY_BIN" -c /nonexistent serve
else
  (cd "$WEB" && bun run generate)
  cd "$ROOT"
  exec cargo run --quiet --bin ruskery -- -c /nonexistent serve
fi
