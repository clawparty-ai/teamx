#!/usr/bin/env bash
# Run the full teamx V1 test suite:
#   1. cargo test          - Rust unit tests (state machines, event ledger)
#   2. tests/smoke.sh      - dual-session happy-path closed loop
#   3. tests/cli-test.sh   - CLI edge & negative cases
#   4. tests/three-member.sh - 3-participant demo (owner+contributor+reviewer)
#   5. tests/concurrency.sh - parallel writers, seq ordering (TC-301)
#   6. plugin typecheck + bundle build
#   7. network-mode mTLS identity test
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "== 1/6 cargo build + unit tests =="
cargo build
cargo test

echo
echo "== 2/6 smoke test (dual-session closed loop) =="
./tests/smoke.sh

echo
echo "== 3/6 CLI edge & negative tests =="
./tests/cli-test.sh

echo
echo "== 4/6 three-member demo test =="
./tests/three-member.sh

echo
echo "== 5/6 concurrency test (seq ordering) =="
./tests/concurrency.sh

echo
echo "== 6/6 plugin typecheck + bundle + unit tests =="
if command -v bun >/dev/null 2>&1; then
  (cd opencode-plugin && bun install >/dev/null 2>&1 && bunx tsc --noEmit && bun run build)
  echo "-- plugin unit tests --"
  TEAMX_AUTO_EXECUTE=1 bun "$ROOT/tests/plugin-unit/auto-execute.test.ts"
else
  echo "bun not found; skipping plugin checks"
fi

echo
echo "== 7/7 network-mode mTLS identity test =="
./tests/mtls-test.sh

echo
echo "ALL TEST SUITES PASSED"
