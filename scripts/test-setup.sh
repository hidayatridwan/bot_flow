#!/usr/bin/env bash
# Create the integration suite's database.
#
# The suite creates and migrates `bot_flow_test` on its own (see crates/api/tests/common/mod.rs),
# so this script exists for CI and for a first local run — somewhere to look when something is
# wrong, rather than a step you must remember. Running it twice is a no-op.
#
# It NEVER touches `bot_flow`. The harness refuses any database whose name does not end in `_test`,
# and this mirrors that refusal one layer out.
set -euo pipefail
cd "$(dirname "$0")/.."

PG_USER="${PG_USER:-bot_flow}"
TEST_DB="${TEST_DB:-bot_flow_test}"

case "$TEST_DB" in
  *_test) ;;
  *) echo "refusing to create '$TEST_DB': the test database name must end in _test" >&2; exit 1 ;;
esac

echo "==> creating $TEST_DB (if it does not exist)"
# `psql -c` runs in its own transaction and CREATE DATABASE cannot; -tc with a guard avoids the
# error path entirely rather than swallowing it.
docker compose exec -T postgres psql -U "$PG_USER" -d postgres -tc \
  "SELECT 1 FROM pg_database WHERE datname = '$TEST_DB'" | grep -q 1 \
  || docker compose exec -T postgres psql -U "$PG_USER" -d postgres -c "CREATE DATABASE $TEST_DB"

echo "==> $TEST_DB ready"
echo
echo "Migrations and the app_user GRANTs are applied by the test harness itself, on first use."
echo "Run the suite with:  cargo test --workspace -- --ignored"
