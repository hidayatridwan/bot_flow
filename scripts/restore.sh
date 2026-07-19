#!/usr/bin/env bash
# Restore a backup taken by scripts/backup.sh.
#
# **This destroys current state as thoroughly as scripts/reset.sh does**, including every dashboard
# login, and then replaces it with the backup's. The prompt names the blast radius for the same
# reason reset.sh does: a prompt that understates it is approved by someone who did not know what
# they were approving.
#
# **Qdrant is not in the backup** (see backup.sh). A restore therefore leaves the collection empty or
# stale, and the system will answer NOTHING while looking perfectly healthy — rows restored, /health
# green, every question refused. The reindex step at the end is not optional, and it is why this
# script prints it rather than assuming you remember.
set -euo pipefail
cd "$(dirname "$0")/.."

DIR="${1:?usage: $(basename "$0") BACKUP_DIR [-y|--yes]}"
shift || true
ASSUME_YES=0
for arg in "$@"; do
  case "$arg" in
    -y|--yes) ASSUME_YES=1 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

PG_USER="${PG_USER:-bot_flow}"
PG_DB="${PG_DB:-bot_flow}"
MINIO_USER="${MINIO_USER:-minio}"
MINIO_PASS="${MINIO_PASS:-minio12345}"
MINIO_BUCKET="${MINIO_BUCKET:-documents}"

[ -f "$DIR/postgres.dump" ] || { echo "no postgres.dump in $DIR" >&2; exit 1; }
[ -d "$DIR/minio/objects" ] || { echo "no minio/objects in $DIR — the bytes are missing, and \
without them 'worker reindex' cannot rebuild anything" >&2; exit 1; }

echo "You are about to restore this backup:"
echo
sed 's/^/    /' "$DIR/MANIFEST" 2>/dev/null || echo "    (no MANIFEST — this backup may be incomplete)"
echo
echo "This will PERMANENTLY REPLACE the current:"
echo "  Postgres ($PG_DB) : every table — tenants, api_keys, accounts, sessions, documents,"
echo "                      conversations, messages, erasures. Current dashboard logins are gone."
echo "  MinIO             : bucket '$MINIO_BUCKET' is mirrored to match the backup exactly,"
echo "                      so objects newer than the backup are DELETED."
echo "  Qdrant            : untouched here, and therefore STALE. See the reindex step at the end."
echo
if [ "$ASSUME_YES" -ne 1 ]; then
  read -r -p "Type 'restore' to continue: " reply
  [ "$reply" = "restore" ] || { echo "aborted."; exit 1; }
fi

echo "==> 1/3 Roles (cluster-level; the RLS grants in the dump reference them by name)"
if [ -f "$DIR/roles.sql" ]; then
  docker compose exec -T postgres psql -U "$PG_USER" -d postgres < "$DIR/roles.sql" >/dev/null 2>&1 \
    || echo "    (roles already exist — fine)"
fi

echo "==> 2/3 Postgres"
docker compose exec -T postgres psql -U "$PG_USER" -d postgres \
  -c "DROP DATABASE IF EXISTS $PG_DB WITH (FORCE);" -c "CREATE DATABASE $PG_DB;" >/dev/null
docker compose exec -T postgres pg_restore -U "$PG_USER" -d "$PG_DB" --no-owner < "$DIR/postgres.dump" \
  >/dev/null 2>&1 || echo "    (pg_restore reported non-fatal notices)"

echo "==> 3/3 MinIO"
# `--remove` is the destructive half and it is required: without it, objects newer than the backup
# survive as orphans with no rows — bytes for documents Postgres has never heard of.
docker compose exec -T minio sh -c "rm -rf /tmp/rs && mkdir -p /tmp/rs"
docker cp "$DIR/minio/objects/." "$(docker compose ps -q minio):/tmp/rs" >/dev/null
docker compose exec -T minio sh -c \
  "mc alias set local http://localhost:9000 $MINIO_USER $MINIO_PASS >/dev/null \
   && mc mb --ignore-existing local/$MINIO_BUCKET >/dev/null \
   && mc mirror --quiet --overwrite --remove /tmp/rs local/$MINIO_BUCKET >/dev/null \
   && rm -rf /tmp/rs"

# Print the comparison rather than assert it. The operator reads it; a script that silently decided
# a restore was "fine" is the failure mode this whole repo is written against.
echo
echo "==> Verifying — backup vs now"
printf '%-14s %-12s %s\n' "" "in backup" "now"
now_tenants=$(docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM tenants' 2>/dev/null | tr -d '\r')
now_docs=$(docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM documents' 2>/dev/null | tr -d '\r')
now_eras=$(docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM erasures' 2>/dev/null | tr -d '\r')
for row in "tenants:$now_tenants" "documents:$now_docs" "erasures:$now_eras"; do
  key="${row%%:*}"; now="${row#*:}"
  was=$(grep -E "^${key}: " "$DIR/MANIFEST" 2>/dev/null | awk '{print $2}')
  printf '%-14s %-12s %s\n' "$key" "${was:-?}" "${now:-?}"
done

cat <<'EOF'

NOT DONE YET. Qdrant is empty or stale, so every question will be refused while the system looks
healthy. Finish the restore:

    # RESTART the api — do not just leave it running. `ensure_collection` runs at STARTUP, so an
    # already-running process will not notice that its collection was dropped underneath it, and
    # the reindex below then fails with "Collection doesn't exist". The drill found this.
    cargo run -p api                       # migrates, recreates the collection
    # STOP the worker if it is running — the reindex driver holds no claim and the two
    # would interleave on the same document (phase 10).
    cargo run -p worker -- reindex         # rebuilds every vector. Billed. Takes a while.
    cargo run -p worker                    # restart normal consumption

Then prove it: ask a question whose answer is in a restored document and check you get a grounded,
cited answer rather than "couldn't find any relevant information". Row counts alone do not prove a
restore — a restore with perfect counts and an empty collection refuses everything.

Note: pre-phase-11 /ingest vectors carry no document_id and no source object. They cannot be rebuilt
and do not come back.
EOF
