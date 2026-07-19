#!/usr/bin/env bash
# Back up the two stores that cannot be rebuilt from anything else.
#
# **What is deliberately NOT backed up, and why it is safe.** Qdrant is *derived data*: phase 10's
# `cargo run -p worker -- reindex` rebuilds the whole collection from MinIO objects + Postgres rows.
# Backing it up would preserve a stale copy of something reconstructible, and the two decisions are
# linked — skipping Qdrant is only safe *because* MinIO is backed up. Lose MinIO and the rebuild is
# impossible.
#
# The cost of that trade, stated rather than buried: recovery means a full re-embed of every chunk of
# every document. That is billed, it takes hours at scale, and pre-phase-11 `/ingest` points cannot
# be rebuilt at all. If that bill is ever worse than the storage, Qdrant's snapshot API
# (`POST /collections/{c}/snapshots`) is the alternative — this script does not use it.
#
# Redis holds rate-limit buckets (losing it grants everyone one fresh minute). RabbitMQ holds
# in-flight events, which MinIO's QUEUE_DIR replays — except the DLQ, whose contents are lost and
# whose recovery is re-upload. Neither is backed up.
#
# NOT a backup *strategy*: no encryption, no offsite copy, no rotation, no scheduling, no PITR.
# This is the mechanism. Where and how often is a deployment decision this repo does not make.
set -euo pipefail
cd "$(dirname "$0")/.."

PG_USER="${PG_USER:-bot_flow}"
PG_DB="${PG_DB:-bot_flow}"
MINIO_USER="${MINIO_USER:-minio}"
MINIO_PASS="${MINIO_PASS:-minio12345}"
MINIO_BUCKET="${MINIO_BUCKET:-documents}"
OUT_ROOT="${OUT_ROOT:-./backups}"

for arg in "$@"; do
  case "$arg" in
    --out) shift; OUT_ROOT="${1:?--out needs a directory}" ;;
    -h|--help)
      echo "Usage: $(basename "$0") [--out DIR]"
      echo "  Backs up Postgres + MinIO into DIR/<UTC timestamp>/. Qdrant is deliberately excluded;"
      echo "  it is rebuilt by 'cargo run -p worker -- reindex' at restore time."
      exit 0 ;;
  esac
done

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
DIR="$OUT_ROOT/$STAMP"
mkdir -p "$DIR/minio"

echo "==> Backing up to $DIR"

# ORDER IS A CORRECTNESS DECISION, not a preference.
#
# Postgres at T0, MinIO over [T0, T1]. A document uploaded inside that window restores as an OBJECT
# WITH NO ROW — an invisible orphan costing storage, and nothing else.
#
# Reverse the order and the same document restores as a ROW WITH NO OBJECT: a `ready` document whose
# bytes are gone, which fails `worker reindex` and presents to the tenant as a document that exists
# and cannot answer. One is a leak; the other is a corruption.
echo "==> 1/2 Postgres (first — see the comment above)"
docker compose exec -T postgres pg_dump -U "$PG_USER" -d "$PG_DB" -Fc > "$DIR/postgres.dump"

# Roles are CLUSTER-level, not database-level, so `pg_dump` does not carry them. Migration 0005
# creates `app_user`, but the dump's GRANTs and RLS policies reference that role BY NAME — restoring
# into a fresh cluster without it fails, or worse, half-succeeds. The two files always travel together.
docker compose exec -T postgres pg_dumpall -U "$PG_USER" --roles-only > "$DIR/roles.sql"

echo "==> 2/2 MinIO"
# **Objects via the S3 API, not MinIO's on-disk layout.** `docker cp /data` copies the erasure-coded
# backend (`xl.meta` shards) — it round-trips only into a byte-identical MinIO version, and is
# useless if the store is ever real S3. `mc mirror` copies the objects themselves, which restore
# anywhere that speaks S3.
#
# The image has `mc` but no `tar`, so this is two steps: mirror inside the container, then lift the
# plain files out. An earlier version used `docker compose cp` with a `|| echo` fallback and the
# drill caught it producing a backup containing ZERO objects while reporting success — the exact
# "looks like it worked" failure this repo exists to prevent. No `|| true` here as a result.
docker compose exec -T minio sh -c \
  "mc alias set local http://localhost:9000 $MINIO_USER $MINIO_PASS >/dev/null \
   && rm -rf /tmp/bk && mc mirror --quiet local/$MINIO_BUCKET /tmp/bk >/dev/null \
   && mc ls --recursive local/$MINIO_BUCKET"  > "$DIR/minio/INVENTORY"
docker cp "$(docker compose ps -q minio):/tmp/bk/." "$DIR/minio/objects" >/dev/null
docker compose exec -T minio sh -c "rm -rf /tmp/bk"

# The manifest is what makes a restore *verifiable* rather than hopeful. A backup that silently
# produced a 0-byte dump looks exactly like one that worked — the same argument reset.sh makes
# about printing its own results.
{
  echo "taken_at_utc: $STAMP"
  echo "git_commit:   $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "collection:   documents_v2   # NOT backed up; rebuilt by 'worker reindex'"
  echo "pg_version:   $(docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SHOW server_version' | tr -d '\r')"
  echo "tenants:      $(docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM tenants' | tr -d '\r')"
  echo "documents:    $(docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM documents' | tr -d '\r')"
  echo "erasures:     $(docker compose exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM erasures' | tr -d '\r')"
  # Counted from the ARCHIVE, never from the inventory. The difference is the difference between
  # "what the store says it holds" and "what this backup actually contains", and only the second
  # one can be restored.
  # Counted from what was ACTUALLY WRITTEN, never from the inventory. The difference between "what
  # the store says it holds" and "what this backup contains" is the difference between a backup and
  # a belief — and the first version of this script reported 1 object while containing none.
  echo "objects:      $(find "$DIR/minio/objects" -type f 2>/dev/null | wc -l | tr -d ' ')"
  echo "object_bytes: $(du -sk "$DIR/minio/objects" 2>/dev/null | awk '{print $1*1024}' || echo 0)"
  echo "dump_bytes:   $(wc -c < "$DIR/postgres.dump" | tr -d ' ')"
} > "$DIR/MANIFEST"

# A backup with no rows or no objects is almost certainly a broken backup, and finding out at
# restore time is finding out too late.
if ! grep -qE '^dump_bytes: +[1-9]' "$DIR/MANIFEST"; then
  echo "REFUSING: the Postgres dump is empty. Backup at $DIR is not usable." >&2
  exit 1
fi
# An object count of 0 while documents exist means the object copy silently did nothing — which is
# precisely what happened the first time this ran, and would have been discovered at restore time.
if grep -qE '^documents: +[1-9]' "$DIR/MANIFEST" && grep -qE '^objects: +0$' "$DIR/MANIFEST"; then
  echo "REFUSING: $DIR has document rows but ZERO objects. Without the bytes, 'worker reindex'" >&2
  echo "          cannot rebuild anything and the restore would answer nothing." >&2
  exit 1
fi

echo
cat "$DIR/MANIFEST"
echo
echo "Done. Restore with:  ./scripts/restore.sh $DIR"
