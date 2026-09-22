#!/usr/bin/env bash
# 平台控制台 E2E（design/014 §5/§6.5）—— 起真实 HTTPS 服务 + 走 API。
#
# 断言：
#   C1 未登录（无 cookie）→ /api/me 401（fail-closed）
#   C2 launch token 登录 → 200 + 下发 HttpOnly cookie
#   C3 租户列表可读（初始为空）
#   C4 建租户（POST /api/tenants，经 stub kubectl 真实调用 create_tenant）
#   C5 平台面只读视图：tokens / admins / audit 均可读
#   C6 回收租户（DELETE）→ 列表回到空
#   C7 service 车道：未在集群内 → /invite 明确报"取不到租户 CA Secret"（不假装成功）
#
# Requires: curl + python3 + openssl（不需要集群：stub kubectl）。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEAMX="${TEAMX:-$ROOT/target/debug/teamx}"
WORK="$(mktemp -d /tmp/teamx-platcon-XXXXXX)"
PORT="${TEAMX_PLATFORM_CONSOLE_PORT:-9532}"
export TEAMX_DB="$WORK/teamx.db"
export TEAMX_HOME="$WORK/home"
unset TEAMX_SERVER_URL TEAMX_PROVISION_FAKE || true
mkdir -p "$TEAMX_HOME" "$WORK/bin"
export PATH="$WORK/bin:$PATH"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
cleanup() {
  [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null || true
  [ "${KEEP:-0}" = 1 ] && { echo "[keep] $WORK"; return; }
  rm -rf "$WORK" /tmp/teamx-platcon-cookie
}
trap cleanup EXIT

# stub kubectl：apply 记录；get secret 返回 NotFound（用于 C7 的负例）
cat > "$WORK/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
LOG="${KUBECTL_LOG:-/dev/null}"; STATE="${KUBECTL_STATE:-/tmp}"; mkdir -p "$STATE"
args=(); skip=0
for a in "$@"; do
  if [ "$skip" = "1" ]; then skip=0; continue; fi
  case "$a" in --kubeconfig|--context) skip=1; continue;; esac
  args+=("$a")
done
set -- "${args[@]}"
verb="$1"; shift || true
case "$verb" in
  apply) y="$(cat)"; printf '%s\n' "$y" > "$STATE/last-apply.yaml"; printf '%s\n' "$y" | grep -E '^kind: ' | sed 's/^kind: //' >> "$LOG"
       # 租户 CA Secret 存成 JSON，供换发读回
       if printf '%s\n' "$y" | grep -q 'ca.crt: |'; then
         printf '%s\n' "$y" | python3 -c '
import sys, json, base64, re
y = sys.stdin.read()
def grab(key):
    m = re.search(key + r": \|\n((?:    .*\n|\n)+)", y)
    if not m: return ""
    return "".join(l[4:] if l.startswith("    ") else l for l in m.group(1).splitlines(True))
json.dump({"data": {k: base64.b64encode(grab(k).encode()).decode() for k in ("ca.crt","ca.key")}}, open(sys.argv[1],"w"))
' "$STATE/ca-secret.json"
       fi
       echo applied;;
  delete) kind="$1"; shift||true; name="$1"; shift||true; echo "delete $kind $name" >> "$LOG"; echo deleted;;
  get) kind="$1"; shift||true
       case "$kind" in
         secret)
           name="$1"; shift||true
           if [ -f "$STATE/ca-secret.json" ] && printf '%s' "$name" | grep -q '^teamx-ca-'; then
             cat "$STATE/ca-secret.json"
           else
             echo "Error from server (NotFound): secrets not found" >&2; exit 1
           fi;;
         deployment,pvc,networkpolicy) echo "";;
         *) echo "{}";;
       esac;;
  *) echo "stub: unhandled $verb" >&2; exit 1;;
esac
STUB
chmod +x "$WORK/bin/kubectl"
export KUBECTL_LOG="$WORK/kubectl.log" KUBECTL_STATE="$WORK/state"
: > "$KUBECTL_LOG"

cd "$WORK"
echo "== platform console E2E (design/014 §5/§6.5) =="
"$TEAMX" init >/dev/null
"$TEAMX" --json team create "PlatCon" --session s1 >/dev/null
TEAM_ID="$(sqlite3 "$TEAMX_DB" "SELECT id FROM teams LIMIT 1;")"

"$TEAMX" platform console --addr 127.0.0.1 --port "$PORT" > "$WORK/pc.log" 2>&1 &
PID=$!
for _ in $(seq 1 40); do grep -q "token=" "$WORK/pc.log" 2>/dev/null && break; sleep 0.3; done
TOKEN="$(sed -n 's/.*token=\([A-Za-z0-9]*\).*/\1/p' "$WORK/pc.log" | head -1)"
[ -n "$TOKEN" ] || { cat "$WORK/pc.log"; fail "C0: 未取到 launch token"; }
BASE="https://127.0.0.1:$PORT"
C="curl -sk --max-time 8"
pass "C0 平台控制台已启动（127.0.0.1:${PORT}）"

# C1: 未登录 → 401
CODE="$($C -o /dev/null -w '%{http_code}' "$BASE/api/me")"
[ "$CODE" = "401" ] || fail "C1: 未登录应为 401，实际 $CODE"
pass "C1 未登录 → 401（fail-closed）"

# C2: 登录
$C -c /tmp/teamx-platcon-cookie -X POST -H 'Content-Type: application/json' \
  -d "{\"token\":\"$TOKEN\"}" "$BASE/api/login" -o /dev/null
grep -q "teamx_platform_token" /tmp/teamx-platcon-cookie || fail "C2: 未下发 cookie"
CODE="$($C -b /tmp/teamx-platcon-cookie -o /dev/null -w '%{http_code}' "$BASE/api/me")"
[ "$CODE" = "200" ] || fail "C2: 登录后 /api/me 应为 200，实际 $CODE"
pass "C2 launch token 登录 → 200 + HttpOnly cookie"

# C3: 租户列表
T="$($C -b /tmp/teamx-platcon-cookie "$BASE/api/tenants")"
echo "$T" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d['ok'] and d['tenants']==[],d" || fail "C3: 初始租户列表应空"
pass "C3 租户列表可读（空）"

# C4: 建租户
R="$($C -b /tmp/teamx-platcon-cookie -X POST -H 'Content-Type: application/json' \
     -d "{\"team\":\"$TEAM_ID\",\"server_image\":\"teamx-server:dev\"}" "$BASE/api/tenants")"
echo "$R" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok'),d;print('  ',d['tenant']['namespace'])" || fail "C4: 建租户失败: $R"
grep -q "kind: Deployment" "$KUBECTL_STATE/last-apply.yaml" || fail "C4: 未 apply 租户 server"
pass "C4 建租户（经真实 create_tenant）"

# C5: 平台面只读
for ep in tokens admins audit; do
  $C -b /tmp/teamx-platcon-cookie "$BASE/api/$ep" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d['ok'],d" || fail "C5: /api/$ep 失败"
done
pass "C5 tokens / admins / audit 可读"

# C7: service 车道负例（集群外无法真正送达租户 server，须明确报错而非假装成功）
S="$($C -b /tmp/teamx-platcon-cookie -X POST -H 'Content-Type: application/json' \
     -d '{"role_desc":"contributor: x"}' "$BASE/api/tenants/$TEAM_ID/invite")"
echo "$S" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok') is not True,d;print('  ',str(d.get('error'))[:80])" || fail "C7: 取不到 CA 时必须失败"
pass "C7 service 车道负例：无法真正送达租户 server 时明确失败（不假装成功）"

# C7b: 造一个成员节点（换发的对象）—— 经 CLI（成员链，非平台面）
INV="$("$TEAMX" --json team invite "contributor: dev" --session s1 --user-name bob \
        --provision-node --provider k8e --region us-west-2 2>/dev/null)"
MID="$(echo "$INV" | python3 -c 'import sys,json;print(json.load(sys.stdin)["member_id"])')"
"$TEAMX" --json node run-jobs >/dev/null 2>&1 || true
"$TEAMX" --json team approve "$MID" --session s1 >/dev/null 2>&1 || true
[ "$(sqlite3 "$TEAMX_DB" "SELECT COUNT(*) FROM nodes WHERE team_id='$TEAM_ID' AND state<>'terminated';")" -ge 1 ] \
  || fail "C7b: 成员节点未创建"
pass "C7b 成员节点就绪（换发对象）"

# C8: 成员证书换发（design/014 §8）—— 经平台控制台入口
R8="$($C -b /tmp/teamx-platcon-cookie -X POST "$BASE/api/tenants/$TEAM_ID/reissue-members")"
echo "$R8" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok'),d;print('  members=%s server_url=%s' % (d.get('members'), d.get('server_url')))" || fail "C8: 换发失败: $R8"
grep -q "teamx.io/kind: rotation-letter" "$KUBECTL_STATE/last-apply.yaml" || fail "C8: 未投递换发信 Secret"
pass "C8 平台控制台触发成员证书换发（租户 CA 签发 + Secret 投递）"

# C6: 回收
D="$($C -b /tmp/teamx-platcon-cookie -X DELETE "$BASE/api/tenants/$TEAM_ID")"
echo "$D" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok'),d" || fail "C6: 回收失败: $D"
T2="$($C -b /tmp/teamx-platcon-cookie "$BASE/api/tenants")"
echo "$T2" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d['tenants']==[],d" || fail "C6: 回收后列表应空"
pass "C6 回收租户（DELETE）"

printf '\nALL PASSED (platform console E2E)\n'
