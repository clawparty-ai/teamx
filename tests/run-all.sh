#!/usr/bin/env bash
# Run the full teamx V1 test suite:
#   1.  cargo test                              - Rust unit tests (state machines, event ledger)
#   2.  tests/smoke.sh                          - dual-session happy-path closed loop
#   3.  tests/cli-test.sh                       - CLI edge & negative cases
#   4.  tests/three-member.sh                   - 3-participant demo (owner+contributor+reviewer)
#   5.  tests/concurrency.sh                    - parallel writers, seq ordering (TC-301)
#   6.  plugin typecheck + bundle + unit tests  - incl. protocol-assets drift check
#   7.  network-mode mTLS identity test
#   8.  network-mode WebSocket push test
#   9.  network-mode cross-network (LAN IP) test
#   10. network-mode reverse-tunnel test
#   11. enterprise activity analytics test (network mode)
#   12. TEAM.md bootstrap test (teamfile-test.sh)
#   13. SOCKS5 proxy test (proxy-test.ts)
#   14. comprehensive tunnel + proxy test (all modes, edge cases, port pool)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "== 1/14 cargo build + unit tests =="
cargo build
cargo test

echo
echo "== 2/14 smoke test (dual-session closed loop) =="
./tests/smoke.sh

echo
echo "== 3/14 CLI edge & negative tests =="
./tests/cli-test.sh

echo
echo "== 4/14 three-member demo test =="
./tests/three-member.sh

echo
echo "== 5/14 concurrency test (seq ordering) =="
./tests/concurrency.sh

echo
echo "== 6/14 plugin typecheck + bundle + unit tests =="
node scripts/generate-grill-protocol.mjs --check
node tests/protocol-assets.test.mjs
if command -v bun >/dev/null 2>&1; then
  (cd opencode-plugin && bun install >/dev/null 2>&1 && bun run typecheck && bun run build)
  if [ -d "$ROOT/../deepseek-harness" ]; then
    (cd dsh-plugin && { npm install >/dev/null 2>&1 || { echo "dsh-plugin npm install failed (dev deps reference ../deepseek-harness sibling — check it exists and is intact)" >&2; exit 1; }; } && npm run typecheck)
  else
    echo "deepseek-harness sibling checkout not found; skipping dsh typecheck"
  fi
  # Check protocol assets are current before building; `npm run build` runs
  # generate:protocols which rewrites the shared opencode + dsh adapters, so
  # verify first to avoid silently overwriting uncommitted changes.
  (cd dsh-plugin && npm run check:protocols)
  (cd dsh-plugin && npm run build)
  echo "-- plugin unit tests --"
  TEAMX_AUTO_EXECUTE=1 bun "$ROOT/tests/plugin-unit/auto-execute.test.ts"
  bun "$ROOT/tests/plugin-unit/ws.test.ts"
  bun "$ROOT/tests/plugin-unit/argv-parse.test.ts"
  bun "$ROOT/tests/plugin-unit/skill-commands-drift.test.ts"
else
  echo "bun not found; skipping plugin checks"
fi

echo
echo "== 7/14 network-mode mTLS identity test =="
./tests/mtls-test.sh

echo
echo "== 8/14 network-mode WebSocket push test =="
if command -v bun >/dev/null 2>&1; then
  bun tests/ws-test.ts
else
  echo "bun not found; skipping WS push test"
fi

echo
echo "== 9/14 network-mode cross-network (LAN IP) test =="
./tests/cross-network.sh

echo
echo "== 10/14 network-mode reverse-tunnel test =="
if command -v bun >/dev/null 2>&1; then
  bun tests/tunnel-test.ts
else
  echo "bun not found; skipping tunnel test"
fi

echo
echo "== 11/14 enterprise activity analytics test (network mode) =="
./tests/activity-test.sh

echo
echo "== 12/14 TEAM.md bootstrap test =="
./tests/teamfile-test.sh

echo
echo "== 13/14 SOCKS5 proxy test =="
if command -v bun >/dev/null 2>&1; then
  bun tests/proxy-test.ts
else
  echo "bun not found; skipping proxy test"
fi

echo
echo "== 14/14 comprehensive tunnel + proxy test =="
if command -v bun >/dev/null 2>&1; then
  bun tests/tunnel-proxy-comprehensive.ts
else
  echo "bun not found; skipping comprehensive tunnel/proxy test"
fi

echo
echo "== 15/16 UI API test =="
if command -v sqlite3 >/dev/null 2>&1 && [ -x target/debug/teamx ]; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/ui-api-test.sh
else
  echo "sqlite3 or debug binary missing; skipping ui-api test"
fi

echo
echo "== 15b Discussion RPC test (network) =="
if [ -x target/debug/teamx ]; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/discussion-rpc-test.sh
else
  echo "debug binary missing; skipping discussion-rpc test"
fi

echo "== 15c Console SSE test (network) =="
if [ -x target/debug/teamx ]; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/console-sse-test.sh
else
  echo "debug binary missing; skipping console-sse test"
fi

echo "== 15c2 Console File panel test (local) =="
if [ -x target/debug/teamx ] && command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/console-files-test.sh
else
  echo "debug binary/curl/python3 missing; skipping console-files test"
fi

echo "== 15d Local-team test (TEAM.md -> single-machine multi-agent) =="
if [ -x target/debug/teamx ] && command -v opencode >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/local-team-test.sh
else
  echo "debug binary or opencode missing; skipping local-team test"
fi

echo
echo "== 15e Local-team server test (self-boot + activate) =="
if [ -x target/debug/teamx ] && command -v opencode >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/local-team-server-test.sh
else
  echo "debug binary or opencode missing; skipping local-team-server test"
fi

echo
echo "== 15f IM outbound notifier (dry-run, closed enum, digest, direct) =="
if [ -x target/debug/teamx ] && command -v sqlite3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/im-notifier-test.sh
else
  echo "debug binary or sqlite3 missing; skipping im-notifier test"
fi

echo
echo "== 15g IM binding test (one-time codes, local CLI) =="
if [ -x target/debug/teamx ]; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/im-binding-test.sh
else
  echo "debug binary missing; skipping im-binding test"
fi

echo
echo "== 15h IM binding RPC test (network, mTLS) =="
if [ -x target/debug/teamx ]; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/im-binding-rpc-test.sh
else
  echo "debug binary missing; skipping im-binding-rpc test"
fi

echo
echo "== 15i Direct listen/ping test (network, P1) =="
if command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1 && [ -x target/debug/teamx ]; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/direct-listen-ping-test.sh
else
  echo "curl/python3 or debug binary missing; skipping direct-listen-ping test"
fi

echo
echo
echo "== 15l IM readonly query test (network, P1) =="
if [ -x target/debug/teamx ] && command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/im-query-test.sh
else
  echo "debug binary/curl/python3 missing; skipping im-query test"
fi

echo
echo "== 15m IM P1 anchor acceptance (4 anchors, network) =="
if [ -x target/debug/teamx ] && command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/im-p1-acceptance.sh
else
  echo "debug binary/curl/python3 missing; skipping im-p1-acceptance test"
fi

echo
echo "== 15n IM gateway process B test (network) =="
if [ -x target/debug/teamx ] && command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/im-gateway-b-test.sh
else
  echo "debug binary/curl/python3 missing; skipping im-gateway-b test"
fi

echo
echo "== 15p IM interactive cards test (network) =="
if [ -x target/debug/teamx ] && command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/im-cards-test.sh
else
  echo "debug binary/curl/python3 missing; skipping im-cards test"
fi

echo
echo "== 15q IM persistent-connection gateway test (network) =="
if [ -x target/debug/teamx ] && command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/im-ws-gateway-test.sh
else
  echo "debug binary/curl/python3 missing; skipping im-ws-gateway test"
fi

echo "== 15r IM fault drills (three-layer interception, network) =="
if [ -x target/debug/teamx ] && command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1 && command -v sqlite3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/im-fault-drill-test.sh
else
  echo "debug binary/curl/python3/sqlite3 missing; skipping im-fault-drill test"
fi

echo "== 16/16 UI e2e (Playwright) test =="
if [ "${TEAMX_RUN_UI_E2E:-0}" = "1" ] && command -v bun >/dev/null 2>&1; then
  (cd tests/ui-e2e && TEAMX_BIN="$ROOT/target/debug/teamx" npx playwright test)
else
  echo "set TEAMX_RUN_UI_E2E=1 (needs Chrome + bun) to run UI e2e; skipping"
fi

echo "== 17/17 node provisioning E2E (design/006, fake provider, zero AWS) =="
./tests/node-provision-test.sh

echo "== 18/18 brainstorm discussion mode E2E (design/007, local) =="
./tests/brainstorm-test.sh

echo "== 18b/18 member workspace reporting E2E (network) =="
if [ -x target/debug/teamx ] && command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/member-workspace-test.sh
else
  echo "debug binary/curl/python3 missing; skipping member-workspace test"
fi

echo "== 18c/18 k8e provider E2E (design/013, stub kubectl, 零集群) =="
if [ -x target/debug/teamx ] && command -v python3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/k8e-provision-test.sh
else
  echo "debug binary/python3 missing; skipping k8e provision test"
fi

echo "== 18d/18 platform plane E2E (design/014 §6/§7：bootstrap token / 管理员 / 审计) =="
if [ -x target/debug/teamx ] && command -v sqlite3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/platform-test.sh
else
  echo "debug binary/sqlite3 missing; skipping platform test"
fi

echo "== 18e/18 tenant full-chain E2E (design/014 第二步：租户 + 成员共存 / CA 独立 / destroy) =="
if [ -x target/debug/teamx ] && command -v sqlite3 >/dev/null 2>&1; then
  TEAMX_BIN="$ROOT/target/debug/teamx" bash tests/tenant-e2e-test.sh
else
  echo "debug binary/sqlite3 missing; skipping tenant E2E"
fi

echo
echo "ALL TEST SUITES PASSED"
