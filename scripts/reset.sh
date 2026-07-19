#!/usr/bin/env bash
# Wipe all bot_flow data to a clean slate: Postgres, MinIO, Qdrant, Redis.
# Dev/local only — assumes the five backing services are up via docker compose.
set -euo pipefail

# Run from repo root regardless of where it's invoked from (needs docker-compose.yml).
cd "$(dirname "$0")/.."

# --- config (override via env if your setup differs) ---
PG_USER="${PG_USER:-bot_flow}"
PG_DB="${PG_DB:-bot_flow}"
PG_TEST_DB="${PG_TEST_DB:-bot_flow_test}"           # the integration suite's database
MINIO_USER="${MINIO_USER:-minio}"
MINIO_PASS="${MINIO_PASS:-minio12345}"
MINIO_BUCKET="${MINIO_BUCKET:-documents}"
QDRANT_HTTP="${QDRANT_HTTP:-http://localhost:6333}"   # REST port, not the 6334 gRPC one
QDRANT_COLLECTION="${QDRANT_COLLECTION:-documents}"
QDRANT_BENCH_COLLECTION="${QDRANT_BENCH_COLLECTION:-eval_bench}"  # crates/eval rebuilds it per run

KEEP_AUTH=0
ASSUME_YES=0
KEEP_TEST_DB=0
for arg in "$@"; do
  case "$arg" in
    --keep-auth)    KEEP_AUTH=1 ;;
    --keep-test-db) KEEP_TEST_DB=1 ;;
    -y|--yes)       ASSUME_YES=1 ;;
    -h|--help)
      echo "Usage: $(basename "$0") [--keep-auth] [--keep-test-db] [-y|--yes]"
      echo "  --keep-auth     keep tenants, api_keys, accounts and sessions"
      echo "                  (wipe only documents/conversations/messages)"
      echo "  --keep-test-db  leave the $PG_TEST_DB database alone"
      echo "  -y, --yes       skip the confirmation prompt"
      exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

# `accounts` and `sessions` are named explicitly even though TRUNCATE ... CASCADE would reach them
# through their FK to `tenants` anyway (verified: the NOTICE lists api_keys, documents, conversations,
# messages, accounts). Naming them is the point — a prompt that says "tenants, api_keys, documents"
# while silently destroying every dashboard login is a prompt that gets approved by someone who did
# not know what they were approving.
if [ "$KEEP_AUTH" -eq 1 ]; then
  TABLES="documents, conversations, messages"
else
  TABLES="tenants, api_keys, accounts, sessions, documents, conversations, messages"
fi

echo "This will PERMANENTLY delete:"
echo "  Postgres ($PG_DB) : TRUNCATE $TABLES"
if [ "$KEEP_TEST_DB" -eq 1 ]; then
  echo "  Postgres ($PG_TEST_DB) : left alone (--keep-test-db)"
else
  echo "  Postgres ($PG_TEST_DB) : TRUNCATE everything, if the database exists"
fi
echo "  MinIO    : all objects in bucket '$MINIO_BUCKET' (bucket + notifications kept)"
echo "  Qdrant   : drop collections '$QDRANT_COLLECTION' and '$QDRANT_BENCH_COLLECTION'"
echo "  Redis    : FLUSHALL"
if [ "$KEEP_AUTH" -ne 1 ]; then
  echo
  echo "  NOTE: that includes every dashboard login (accounts + sessions) and every"
  echo "        sk_/pk_ key. Use --keep-auth to keep them and wipe only the documents."
fi
echo
if [ "$ASSUME_YES" -ne 1 ]; then
  read -r -p "Type 'yes' to continue: " reply
  [ "$reply" = "yes" ] || { echo "aborted."; exit 1; }
fi

echo "==> Postgres ($PG_DB): truncating $TABLES"
docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_DB" \
  -c "TRUNCATE $TABLES RESTART IDENTITY CASCADE;"

if [ "$KEEP_TEST_DB" -ne 1 ]; then
  # The integration suite creates a tenant per test and only sweeps debris older than an hour, so
  # this database accumulates. Truncating (not dropping) keeps _sqlx_migrations, so the next
  # `cargo test -- --ignored` does not have to re-run every migration.
  if docker compose exec -T postgres psql -U "$PG_USER" -d postgres -tAc \
       "SELECT 1 FROM pg_database WHERE datname = '$PG_TEST_DB'" | grep -q 1; then
    echo "==> Postgres ($PG_TEST_DB): truncating the integration suite's tenants"
    docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_TEST_DB" \
      -c "TRUNCATE tenants RESTART IDENTITY CASCADE;" >/dev/null
  else
    echo "==> Postgres ($PG_TEST_DB): does not exist yet, nothing to do"
  fi
fi

echo "==> MinIO: clearing objects (bucket + event binding preserved)"
docker compose exec -T minio sh -c \
  "mc alias set local http://localhost:9000 $MINIO_USER $MINIO_PASS >/dev/null \
   && mc rm --recursive --force local/$MINIO_BUCKET >/dev/null 2>&1 || true"

echo "==> Qdrant: dropping collections '$QDRANT_COLLECTION' and '$QDRANT_BENCH_COLLECTION'"
curl -sS -X DELETE "$QDRANT_HTTP/collections/$QDRANT_COLLECTION" >/dev/null || true
curl -sS -X DELETE "$QDRANT_HTTP/collections/$QDRANT_BENCH_COLLECTION" >/dev/null || true

echo "==> Redis: FLUSHALL"
docker compose exec -T redis redis-cli FLUSHALL >/dev/null

# Show the result rather than asserting it. A reset that quietly did nothing looks exactly like one
# that worked, and this is a script people run precisely when they have stopped trusting the state.
echo
echo "==> Verifying"
docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc \
  "SELECT 'postgres: ' || (SELECT count(*) FROM tenants) || ' tenants, '
                        || (SELECT count(*) FROM documents) || ' documents, '
                        || (SELECT count(*) FROM accounts) || ' accounts;'"
echo -n "qdrant:   "
curl -sS "$QDRANT_HTTP/collections" \
  | python3 -c "import sys,json;c=[x['name'] for x in json.load(sys.stdin)['result']['collections']];print(c or 'no collections (recreated at api startup)')" \
  2>/dev/null || echo "(could not read $QDRANT_HTTP)"

echo
echo "Done. Restart the binaries so the collection + bucket are recreated:"
echo "    cargo run -p api    # expect log: collection 'documents' created (dim=1536, cosine) + tenant_id index"
echo "    cargo run -p worker"
if [ "$KEEP_AUTH" -eq 1 ]; then
  echo "Your tenants, keys and logins were kept — the same sk_ still works."
else
  echo "tenants, keys, accounts and sessions were wiped. Create a tenant to get a fresh sk_:"
  echo "    curl -sX POST localhost:3000/admin/tenants -H \"authorization: Bearer \$ADMIN_API_KEY\" \\"
  echo "         -H 'content-type: application/json' -d '{\"id\":\"demo\",\"name\":\"Demo Co\"}'"
  echo "  or sign up at http://localhost:5173/signup if you are running the dashboard."
fi
