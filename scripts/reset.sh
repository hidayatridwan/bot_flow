#!/usr/bin/env bash
# Wipe all bot_flow data to a clean slate: Postgres, MinIO, Qdrant, Redis.
# Dev/local only — assumes the five backing services are up via docker compose.
set -euo pipefail

# Run from repo root regardless of where it's invoked from (needs docker-compose.yml).
cd "$(dirname "$0")/.."

# --- config (override via env if your setup differs) ---
PG_USER="${PG_USER:-bot_flow}"
PG_DB="${PG_DB:-bot_flow}"
MINIO_USER="${MINIO_USER:-minio}"
MINIO_PASS="${MINIO_PASS:-minio12345}"
MINIO_BUCKET="${MINIO_BUCKET:-documents}"
QDRANT_HTTP="${QDRANT_HTTP:-http://localhost:6333}"   # REST port, not the 6334 gRPC one
QDRANT_COLLECTION="${QDRANT_COLLECTION:-documents}"

KEEP_AUTH=0
ASSUME_YES=0
for arg in "$@"; do
  case "$arg" in
    --keep-auth) KEEP_AUTH=1 ;;
    -y|--yes)    ASSUME_YES=1 ;;
    -h|--help)
      echo "Usage: $(basename "$0") [--keep-auth] [-y|--yes]"
      echo "  --keep-auth  keep tenants + api_keys (wipe only documents/conversations/messages)"
      echo "  -y, --yes    skip the confirmation prompt"
      exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

if [ "$KEEP_AUTH" -eq 1 ]; then
  TABLES="documents, conversations, messages"
else
  TABLES="tenants, api_keys, documents, conversations, messages"
fi

echo "This will PERMANENTLY delete:"
echo "  Postgres : TRUNCATE $TABLES"
echo "  MinIO    : all objects in bucket '$MINIO_BUCKET' (bucket + notifications kept)"
echo "  Qdrant   : drop collection '$QDRANT_COLLECTION'"
echo "  Redis    : FLUSHALL"
echo
if [ "$ASSUME_YES" -ne 1 ]; then
  read -r -p "Type 'yes' to continue: " reply
  [ "$reply" = "yes" ] || { echo "aborted."; exit 1; }
fi

echo "==> Postgres: truncating $TABLES"
docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_DB" \
  -c "TRUNCATE $TABLES RESTART IDENTITY CASCADE;"

echo "==> MinIO: clearing objects (bucket + event binding preserved)"
docker compose exec -T minio sh -c \
  "mc alias set local http://localhost:9000 $MINIO_USER $MINIO_PASS >/dev/null \
   && mc rm --recursive --force local/$MINIO_BUCKET >/dev/null 2>&1 || true"

echo "==> Qdrant: dropping collection '$QDRANT_COLLECTION'"
curl -sS -X DELETE "$QDRANT_HTTP/collections/$QDRANT_COLLECTION" >/dev/null || true

echo "==> Redis: FLUSHALL"
docker compose exec -T redis redis-cli FLUSHALL >/dev/null

echo
echo "Done. Restart the binaries so the collection + bucket are recreated:"
echo "    cargo run -p api    # expect log: collection 'documents' created (dim=1536, cosine) + tenant_id index"
echo "    cargo run -p worker"
[ "$KEEP_AUTH" -eq 1 ] || echo "tenants + api_keys were wiped — re-create a tenant for a fresh sk_ key."
