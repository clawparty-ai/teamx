#!/usr/bin/env bash
# teamx 真实模型级验收（headless）：
# 用 opencode run --agent teamx 让真实模型走一遍 teamx_* 工具链，
# 验证模型确实通过插件调用了 teamx CLI 并写入账本。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DB="$(mktemp /tmp/teamx-accept-XXXXXX).db)"
DB="${DB%)}"
export TEAMX_DB="$DB"
MODEL="${TEAMX_ACCEPT_MODEL:-opencode/deepseek-v4-flash-free}"
TEAMX_BIN_DIR="${TEAMX_BIN_DIR:-$HOME/.local/bin}"
export PATH="$TEAMX_BIN_DIR:$PATH"

echo "== 模型级验收：opencode run --agent teamx =="
echo "model: $MODEL  db: $DB"

echo "--- 步骤1：创建团队（真实模型调用 teamx_create_team）---"
TEAMX_DB="$DB" opencode run \
  --agent teamx \
  --model "$MODEL" \
  --dir "$ROOT/demo/workspace" \
  "创建一个团队，名字叫「验收测试组」，目标是验证团队协作。" 2>&1 | tail -20 || true

echo "--- 验证账本 ---"
python3 - <<'PY'
import sqlite3, os
db = os.environ["TEAMX_DB"]
conn = sqlite3.connect(db)
teams = conn.execute("SELECT name, state FROM teams").fetchall()
events = conn.execute("SELECT type FROM events ORDER BY seq").fetchall()
members = conn.execute("SELECT display_name, role, state FROM members").fetchall()
print("teams:", teams)
print("members:", members)
print("events:", [e[0] for e in events])
assert teams, "FAIL: no team created by the model"
assert any(t[0] == "验收测试组" for t in teams), f"FAIL: team name mismatch: {teams}"
assert any(e[0] == "team.created" for e in events), "FAIL: no team.created event"
print("ACCEPTANCE PASS: model invoked teamx_create_team through the plugin")
PY

rm -f "$DB" "$DB-wal" "$DB-shm"
echo "cleaned up $DB"
