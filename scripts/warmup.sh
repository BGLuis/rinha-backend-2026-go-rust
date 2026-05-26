#!/bin/sh
set -eu

BASE_URL="${BASE_URL:-http://localhost:9999}"
READY_URL="${BASE_URL}/ready"
READY_RETRIES="${READY_RETRIES:-120}"
READY_SLEEP="${READY_SLEEP:-0.25}"

echo "warming up stack via ${BASE_URL}"

i=1
while [ "${i}" -le "${READY_RETRIES}" ]; do
  if curl -fsS --max-time 1 "${READY_URL}" >/dev/null; then
    break
  fi
  if [ "${i}" -eq "${READY_RETRIES}" ]; then
    echo "ready check failed after ${READY_RETRIES} attempts" >&2
    exit 1
  fi
  sleep "${READY_SLEEP}"
  i=$((i + 1))
done

# Run the diverse k6 warmup script!
docker run --rm --network host -v "$(pwd)/test:/test" grafana/k6 run /test/warmup.js

echo "warmup complete"
