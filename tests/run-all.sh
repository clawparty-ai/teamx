#!/usr/bin/env bash
# Run the full teamx V1 test suite:
#   1. cargo test          - Rust unit tests (state machines, event ledger)
#   2. tests/smoke.sh      - dual-session happy-path closed loop
#   3. tests/cli-test.sh   - CLI edge & negative cases
#   4. tests/three-member.sh - 3-participant demo (owner+contributor+reviewer)
#   5. tests/concurrency.sh - parallel writers, seq ordering (TC-301)
#   6. plugin typecheck + bundle build
#   7. network-mode mTLS identity test
#   8. network-mode WebSocket push test
#   9. network-mode cross-network (LAN IP) test
#   10. network-mode reverse-tunnel test
#   11. TEAM.md bootstrap test (teamfile-test.sh)
#   12. SOCKS5 proxy test (proxy-test.ts)
#   13. comprehensive tunnel + proxy test (all modes, edge cases, port pool)
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
node scripts/generate-grill-protocol.mjs --check
node tests/protocol-assets.test.mjs
if command -v bun >/dev/null 2>&1; then
  (cd opencode-plugin && bun install >/dev/null 2>&1 && bun run typecheck && bun run build)
  if [ -d "$ROOT/../deepseek-harness" ]; then
    (cd dsh-plugin && npm run typecheck)
  else
    echo "deepseek-harness sibling checkout not found; skipping dsh typecheck"
  fi
  (cd dsh-plugin && npm run build)
  echo "-- plugin unit tests --"
  TEAMX_AUTO_EXECUTE=1 bun "$ROOT/tests/plugin-unit/auto-execute.test.ts"
  bun "$ROOT/tests/plugin-unit/ws.test.ts"
else
  echo "bun not found; skipping plugin checks"
fi

echo
echo "== 7/8 network-mode mTLS identity test =="
./tests/mtls-test.sh

echo
echo "== 8/8 network-mode WebSocket push test =="
if command -v bun >/dev/null 2>&1; then
  bun tests/ws-test.ts
else
  echo "bun not found; skipping WS push test"
fi

echo
echo "== 9/9 network-mode cross-network (LAN IP) test =="
./tests/cross-network.sh

echo
echo "== 10/10 network-mode reverse-tunnel test =="
if command -v bun >/dev/null 2>&1; then
  bun tests/tunnel-test.ts
else
  echo "bun not found; skipping tunnel test"
fi

echo
echo "== 11/11 TEAM.md bootstrap test =="
./tests/teamfile-test.sh

echo
echo "== 12/12 SOCKS5 proxy test =="
if command -v bun >/dev/null 2>&1; then
  bun tests/proxy-test.ts
else
  echo "bun not found; skipping proxy test"
fi

echo
echo "== 13/13 comprehensive tunnel + proxy test =="
if command -v bun >/dev/null 2>&1; then
  bun tests/tunnel-proxy-comprehensive.ts
else
  echo "bun not found; skipping comprehensive tunnel/proxy test"
fi

echo
echo "ALL TEST SUITES PASSED"
