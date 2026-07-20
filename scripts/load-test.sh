#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8100}"
TARGET_PATH="${TARGET_PATH:-/api/health}"
CONCURRENCY="${CONCURRENCY:-64}"
REQUESTS="${REQUESTS:-5000}"

if ! command -v hey >/dev/null 2>&1; then
  echo "hey is required. Install it with: brew install hey" >&2
  exit 1
fi

curl -fsS "${BASE_URL}/api/health" >/dev/null

echo "Target: ${BASE_URL}${TARGET_PATH}"
echo "Requests: ${REQUESTS}"
echo "Concurrency: ${CONCURRENCY}"
hey -n "${REQUESTS}" -c "${CONCURRENCY}" "${BASE_URL}${TARGET_PATH}"

echo
echo "Runtime metrics:"
curl -fsS "${BASE_URL}/api/metrics"
echo

