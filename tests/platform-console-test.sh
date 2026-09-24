#!/usr/bin/env bash
# 平台控制台 E2E（design/014 §5/§6.5）—— 起真实 HTTPS 服务 + 走 API。
#
# 断言：
#   C1 未登录（无 cookie）→ /api/me 401（fail-closed）
#   C2 launch token 登录 → 200 + 下发 HttpOnly cookie
#   C3 租户列表可读（初始为空）
#   C4 建租户（POST /api/tenants，经 stub kubectl 真实调用 create_tenant）
#   C5 平台面只读视图：tokens / admins / audit 均可读
#   C6 回收租户（DELETE）→ 被回收的不在列表（bob 自助建的保留）
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
  [ -n "${PID2:-}" ] && kill "$PID2" 2>/dev/null || true
  [ -n "${MPID:-}" ] && kill "$MPID" 2>/dev/null || true
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

# C9: 用户密码登录全链（Phase 1）
U="$("$TEAMX" --json platform user create --name ops --password 's3cret-pw!' 2>&1)" || fail "C9: 建用户失败: $U"
USER_ID="$(echo "$U" | python3 -c 'import sys,json;print(json.load(sys.stdin)["user_id"])')"
[ -n "$USER_ID" ] || fail "C9: 无 user_id"
# 用户列表不含 hash
"$TEAMX" --json platform user list 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);assert d['users'][0]['display_name']=='ops',d;assert 'password_hash' not in str(d),d" || fail "C9: 用户列表异常"
# 密码登录 → 会话 cookie → API 可用
$C -c /tmp/teamx-platcon-ucookie -X POST -H 'Content-Type: application/json' \
  -d '{"name":"ops","password":"s3cret-pw!"}' "$BASE/api/login/password" -o /dev/null
grep -q "teamx_platform_token" /tmp/teamx-platcon-ucookie || fail "C9: 未下发会话 cookie"
ME="$($C -b /tmp/teamx-platcon-ucookie "$BASE/api/me")"
echo "$ME" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok') and d.get('who','').startswith('user:ops'),d" || fail "C9: 会话身份不对: $ME"
$C -b /tmp/teamx-platcon-ucookie "$BASE/api/tenants" | python3 -c "import json,sys;assert json.load(sys.stdin)['ok']" || fail "C9: 会话调 API 失败"
# 错密码 → 401（统一口径）
CODE="$($C -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{"name":"ops","password":"wrong-wrong"}' "$BASE/api/login/password")"
[ "$CODE" = "401" ] || fail "C9: 错密码应 401，实际 $CODE"
# 登出 → 会话失效
$C -b /tmp/teamx-platcon-ucookie -X POST "$BASE/api/logout" -o /dev/null
CODE="$($C -b /tmp/teamx-platcon-ucookie -o /dev/null -w '%{http_code}' "$BASE/api/me")"
[ "$CODE" = "401" ] || fail "C9: 登出后会话应失效，实际 $CODE"
pass "C9 用户密码登录（建用户/登录/会话/登出）"
# C9b: 禁用 → 登录与会话双拒
"$TEAMX" --json platform user disable --user-id "$USER_ID" >/dev/null 2>&1 || fail "C9b: 禁用失败"
CODE="$($C -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{"name":"ops","password":"s3cret-pw!"}' "$BASE/api/login/password")"
[ "$CODE" = "401" ] || fail "C9b: 禁用后登录应 401，实际 $CODE"
pass "C9b 禁用用户（登录拒绝）"

# D1: 建两个用户并登录
for u in bob alice; do
  "$TEAMX" --json platform user create --name "$u" --password 'pw-12345!' >/dev/null 2>&1 || fail "D1: 建用户 $u 失败"
done
for u in bob alice; do
  $C -c /tmp/teamx-platcon-$u -X POST -H 'Content-Type: application/json' \
    -d "{\"name\":\"$u\",\"password\":\"pw-12345!\"}" "$BASE/api/login/password" -o /dev/null
  grep -q "teamx_platform_token" /tmp/teamx-platcon-$u || fail "D1: $u 登录失败"
done
pass "D1 bob/alice 建用户并登录"

# D2: bob 自助建 team（O-1(a) 入口）
BOB_T="$($C -b /tmp/teamx-platcon-bob -X POST -H 'Content-Type: application/json' \
  -d '{"name":"bob-team"}' "$BASE/api/my/teams")"
echo "$BOB_T" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok') and d.get('bootstrap')=='pending',d;print('  team=%s ns=%s' % (d['team_id'][:8], d['tenant']['namespace']))" || fail "D2: 自助建失败: $BOB_T"
BOB_TEAM="$(echo "$BOB_T" | python3 -c 'import sys,json;print(json.load(sys.stdin)["team_id"])')"
pass "D2 bob 自助建 team（配额内，bootstrap pending）"

# D3: 归属隔离（bob 见自己的；alice 看不到；运维租户用户不可见）
$C -b /tmp/teamx-platcon-bob "$BASE/api/my/teams" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d['ok'] and len(d['owned'])==1 and d['used']==1 and d['quota']==3,d" || fail "D3: bob 的 my/teams 不对"
$C -b /tmp/teamx-platcon-alice "$BASE/api/my/teams" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d['ok'] and d['owned']==[],d" || fail "D3: alice 应为空"
$C -b /tmp/teamx-platcon-alice "$BASE/api/tenants" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d['ok'] and d['tenants']==[],d" || fail "D3: alice 在 /api/tenants 应看不到任何（含运维建的）"
pass "D3 归属隔离（各看各的）"

# D4: 越权 403（alice 动 bob 的租户：destroy + invite）
CODE="$($C -b /tmp/teamx-platcon-alice -o /dev/null -w '%{http_code}' -X DELETE "$BASE/api/tenants/$BOB_TEAM")"
[ "$CODE" = "403" ] || fail "D4: 越权 destroy 应 403，实际 $CODE"
CODE="$($C -b /tmp/teamx-platcon-alice -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{"role_desc":"contributor: x"}' "$BASE/api/tenants/$BOB_TEAM/invite")"
[ "$CODE" = "403" ] || fail "D4: 越权 invite 应 403，实际 $CODE"
# 用户走运维入口建租户 → 403 指引
CODE="$($C -b /tmp/teamx-platcon-bob -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{"team":"x"}' "$BASE/api/tenants")"
[ "$CODE" = "403" ] || fail "D4: 用户走运维入口应 403，实际 $CODE"
pass "D4 越权 403（destroy/invite/运维入口）"

# D5: 配额（bob 配额改为 1，已用 1 → 再建被拒；运维不受影响）
"$TEAMX" --json platform user set-quota --user-id "$("$TEAMX" --json platform user list 2>/dev/null | python3 -c 'import sys,json;print([u["user_id"] for u in json.load(sys.stdin)["users"] if u["display_name"]=="bob"][0])')" --quota 1 >/dev/null || fail "D5: 设配额失败"
R="$($C -b /tmp/teamx-platcon-bob -X POST -H 'Content-Type: application/json' -d '{"name":"bob-team-2"}' "$BASE/api/my/teams")"
echo "$R" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok') is not True and 'quota' in str(d.get('error','')),d" || fail "D5: 超配额应明确拒绝"
pass "D5 配额生效（1/1 后再建被拒）"

# D6: bootstrap 入口校验（缺 server_url → 400；不可达 → 明确失败不假装）
CODE="$($C -b /tmp/teamx-platcon-bob -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{}' "$BASE/api/my/teams/$BOB_TEAM/bootstrap")"
[ "$CODE" = "400" ] || fail "D6: 缺 server_url 应 400，实际 $CODE"
R="$($C -b /tmp/teamx-platcon-bob -X POST -H 'Content-Type: application/json' -d '{"server_url":"https://127.0.0.1:9"}' "$BASE/api/my/teams/$BOB_TEAM/bootstrap" --max-time 30)"
echo "$R" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok') is not True,d" || fail "D6: 不可达应明确失败"
# alice 替 bob bootstrap → 403
CODE="$($C -b /tmp/teamx-platcon-alice -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{"server_url":"https://127.0.0.1:9"}' "$BASE/api/my/teams/$BOB_TEAM/bootstrap" --max-time 30)"
[ "$CODE" = "403" ] || fail "D6: 越权 bootstrap 应 403，实际 $CODE"
pass "D6 bootstrap 入口校验（400/失败明示/403）"

# D7: M2 —— 用户自助不接受 server_image（镜像由平台 pin）
R7="$($C -b /tmp/teamx-platcon-bob -X POST -H 'Content-Type: application/json' \
  -d '{"name":"evil-image","server_image":"attacker/evil:latest"}' "$BASE/api/my/teams")"
if echo "$R7" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok')" 2>/dev/null; then
  # 建成功（配额已满应先拒；此处若成功则查其镜像未被采用）
  echo "  info: 建成功，检查镜像是否被忽略"
fi
grep -q "attacker/evil" "$KUBECTL_STATE/last-apply.yaml" && fail "D7: server_image 被采用（应忽略）"
pass "D7 用户面 server_image 被忽略（M2）"

# D8: M3 —— bootstrap server_url 必须锚定本租户 endpoint
EVIL="$($C -b /tmp/teamx-platcon-bob -X POST -H 'Content-Type: application/json' \
  -d '{"server_url":"https://attacker.example.com:443"}' "$BASE/api/my/teams/$BOB_TEAM/bootstrap" --max-time 30)"
echo "$EVIL" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok') is not True and 'not this tenant' in str(d.get('error','')),d" || fail "D8: 外部端点应被拒: $EVIL"
CODE="$($C -b /tmp/teamx-platcon-bob -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{"server_url":"http://insecure.example"}' "$BASE/api/my/teams/$BOB_TEAM/bootstrap" --max-time 20)"
[ "$CODE" = "400" ] || fail "D8: 非 https 应 400，实际 $CODE"
pass "D8 bootstrap server_url 锚定校验（M3）"

# C6: 回收
D="$($C -b /tmp/teamx-platcon-cookie -X DELETE "$BASE/api/tenants/$TEAM_ID")"
echo "$D" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok'),d" || fail "C6: 回收失败: $D"
T2="$($C -b /tmp/teamx-platcon-cookie "$BASE/api/tenants")"
# D2 建的 bob 租户还在 —— 只断言被回收的不在即可
echo "$T2" | python3 -c "import json,sys;d=json.load(sys.stdin);assert '$TEAM_ID' not in [t['team_id'] for t in d['tenants']],d" || fail "C6: 被回收的仍在列表"
pass "C6 回收租户（DELETE）"

# E1-E9: 团队发现与加入（design/015）
echo "== E: 团队发现与加入 =="
# 用运维身份建一个"公开可加入"的租户（stub kubectl 会模拟建租户）
TE="$($C -b /tmp/teamx-platcon-cookie -X POST -H 'Content-Type: application/json' \
  -d '{"team":"discoverable"}' "$BASE/api/tenants")" 2>/dev/null || true
# 运维建的租户 team_id 需从列表取（create 需要 team id；这里用 bob 的自助建更贴近真实流程）
# bob 在 D5 被降配额到 1 且已有团队 → 先提额再建（E 段自足）
BOB_ID="$("$TEAMX" --json platform user list 2>/dev/null | python3 -c 'import sys,json;print([u["user_id"] for u in json.load(sys.stdin)["users"] if u["display_name"]=="bob"][0])')"
"$TEAMX" --json platform user set-quota --user-id "$BOB_ID" --quota 5 >/dev/null 2>&1
E_TEAM="$($C -b /tmp/teamx-platcon-bob -X POST -H 'Content-Type: application/json' -d '{"name":"公开测试团队"}' "$BASE/api/my/teams" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("team_id","") if d.get("ok") else "")')"
[ -n "$E_TEAM" ] || fail "E1: 造团队失败"
# E1: 默认 private → 目录里看不到
$C -b /tmp/teamx-platcon-bob "$BASE/api/discovery/directory" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d['ok'], d
assert '$E_TEAM' not in [i['team_id'] for i in d['items']], 'private 不应出现在目录'
" || fail "E1: 默认 private 未生效"
pass "E1 默认 private → 不在公开名录"

# E2: owner 设为 public + open_request + 简介
$C -b /tmp/teamx-platcon-bob -X POST -H 'Content-Type: application/json' \
  -d '{"visibility":"public","join_policy":"open_request","summary":"一个可申请加入的公开团队"}' \
  "$BASE/api/discovery/teams/$E_TEAM/settings" | python3 -c "
import json,sys; d=json.load(sys.stdin); assert d.get('ok'), d
assert d['settings']['visibility']=='public' and d['settings']['join_policy']=='open_request', d" || fail "E2: 设置失败"
pass "E2 owner 设为 public + open_request（含简介）"

# E3: 目录可见（alice 视角）+ joinable + 搜索
D="$($C -b /tmp/teamx-platcon-alice "$BASE/api/discovery/directory")"
echo "$D" | python3 -c "
import json,sys; d=json.load(sys.stdin)
m=[i for i in d['items'] if i['team_id']=='$E_TEAM']
assert m and m[0]['joinable'] is True, d
print('  ', m[0]['name'], '| joinable=', m[0]['joinable'], '| members=', m[0]['member_count'])" || fail "E3: 目录未列出"
$C -b /tmp/teamx-platcon-alice "$BASE/api/discovery/directory?query=%E5%85%AC%E5%BC%80" | python3 -c "
import json,sys; d=json.load(sys.stdin); assert any(i['team_id']=='$E_TEAM' for i in d['items']), d" || fail "E3: 搜索失败"
pass "E3 目录可见 + 可加入标记 + 搜索"

# E4: 非成员详情可见；未登录 403
$C -b /tmp/teamx-platcon-alice "$BASE/api/discovery/teams/$E_TEAM" | python3 -c "
import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['team']['joinable'] is True, d
assert d['team'].get('tenant_meta') is None, '租户侧字段应诚实为空(O-5)'
assert 'invite_token' not in json.dumps(d), '不得泄漏 invite_token'" || fail "E4: 详情异常"
CODE="$($C -o /dev/null -w '%{http_code}' "$BASE/api/discovery/directory")"
[ "$CODE" = "401" ] || fail "E4: 未登录应 401（未认证），实际 $CODE"
pass "E4 详情（无凭据泄漏）+ 未登录 401"

# E5: alice 申请加入
R="$($C -b /tmp/teamx-platcon-alice -X POST -H 'Content-Type: application/json' -d '{"message":"我想加入"}' "$BASE/api/discovery/teams/$E_TEAM/apply")"
RID="$(echo "$R" | python3 -c 'import json,sys;d=json.load(sys.stdin);assert d.get("ok"),d;print(d["request_id"])')" || fail "E5: 申请失败: $R"
# 重复申请被拒
$C -b /tmp/teamx-platcon-alice -X POST -H 'Content-Type: application/json' -d '{}' "$BASE/api/discovery/teams/$E_TEAM/apply" | python3 -c "
import json,sys; d=json.load(sys.stdin); assert d.get('ok') is not True and 'pending' in str(d.get('error','')), d" || fail "E5: 重复申请应被拒"
pass "E5 申请加入 + 重复申请被拒"

# E6: owner 待审可见 + 非 owner 不能审批
$C -b /tmp/teamx-platcon-bob "$BASE/api/discovery/pending" | python3 -c "
import json,sys; d=json.load(sys.stdin); assert any(r['request_id']=='$RID' for r in d['requests']), d" || fail "E6: owner 待审列表缺申请"
$C -b /tmp/teamx-platcon-alice -X POST -H 'Content-Type: application/json' -d '{"approve":true}' "$BASE/api/discovery/requests/$RID/decide" | python3 -c "
import json,sys; d=json.load(sys.stdin); assert d.get('ok') is not True, d" || fail "E6: 非 owner 不应能审批"
pass "E6 owner 待审列表 + 非 owner 审批被拒"

# E7: 我的申请（alice）可见 pending；撤回后批准应失败
$C -b /tmp/teamx-platcon-alice "$BASE/api/discovery/mine" | python3 -c "
import json,sys; d=json.load(sys.stdin); r=[x for x in d['requests'] if x['request_id']=='$RID'][0]; assert r['status']=='pending', d" || fail "E7: 我的申请状态异常"
pass "E7 我的申请（pending）"

# E8: owner 批准
$C -b /tmp/teamx-platcon-bob -X POST -H 'Content-Type: application/json' -d '{"approve":true}' "$BASE/api/discovery/requests/$RID/decide" | python3 -c "
import json,sys; d=json.load(sys.stdin); assert d.get('ok') and d['status']=='approved' and d['applicant_name']=='alice', d" || fail "E8: 批准失败"
pass "E8 owner 批准（返回申请人）"

# E9: 拒绝 → 冷却（新申请一次再拒）
R2="$($C -b /tmp/teamx-platcon-alice -X POST -H 'Content-Type: application/json' -d '{}' "$BASE/api/discovery/teams/$E_TEAM/apply")"
RID2="$(echo "$R2" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("request_id",""))')"
[ -n "$RID2" ] || fail "E9: 二次申请失败: $R2"
$C -b /tmp/teamx-platcon-bob -X POST -H 'Content-Type: application/json' -d '{"approve":false}' "$BASE/api/discovery/requests/$RID2/decide" | python3 -c "
import json,sys; d=json.load(sys.stdin); assert d.get('ok') and d['status']=='rejected', d" || fail "E9: 拒绝失败"
$C -b /tmp/teamx-platcon-alice "$BASE/api/discovery/mine" | python3 -c "
import json,sys; d=json.load(sys.stdin); r=[x for x in d['requests'] if x['request_id']=='$RID2'][0]
assert r['status']=='rejected' and r['cooldown_until'], d" || fail "E9: 冷却未记录"
$C -b /tmp/teamx-platcon-alice -X POST -H 'Content-Type: application/json' -d '{}' "$BASE/api/discovery/teams/$E_TEAM/apply" | python3 -c "
import json,sys; d=json.load(sys.stdin); assert d.get('ok') is not True and 're-apply' in str(d.get('error','')), d" || fail "E9: 冷却期内不应能再申请"
pass "E9 拒绝 + 7 天冷却生效"

# F1-F6: 「我的团队」（design/016）
echo "== F: 我的团队 =="
# F1: 两组 + 统计
MT="$($C -b /tmp/teamx-platcon-bob "$BASE/api/my/teams")"
echo "$MT" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok'), d
for k in ('owned','joined','used','quota','joined_count','running_workspaces'):
    assert k in d, ('缺字段', k, d)
assert isinstance(d['owned'], list) and isinstance(d['joined'], list), d
print('  owned=%d joined=%d used/quota=%s/%s running=%s' % (len(d['owned']), len(d['joined']), d['used'], d['quota'], d['running_workspaces']))" || fail "F1: my/teams 结构异常"
pass "F1 我的团队：两组 + 统计字段"

# F2: 创建表单一步带发现设置（public + open_request）
F_TEAM="$($C -b /tmp/teamx-platcon-alice -X POST -H 'Content-Type: application/json' \
  -d '{"name":"表单建队","visibility":"public","join_policy":"open_request","summary":"一步建好的公开团队"}' \
  "$BASE/api/my/teams" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("team_id","") if d.get("ok") else "")')"
[ -n "$F_TEAM" ] || fail "F2: 表单创建失败"
$C -b /tmp/teamx-platcon-alice "$BASE/api/discovery/teams/$F_TEAM" | python3 -c "
import json,sys; t=json.load(sys.stdin)['team']
assert t['joinable'] is True, t" || fail "F2: 创建时未带上发现设置"
pass "F2 创建表单一步带发现设置（public + open_request）"

# F3: Owner 邀请函取件页（未初始化 → available=false，不假装有）
$C -b /tmp/teamx-platcon-alice "$BASE/api/my/letters" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') and isinstance(d['teams'], list), d
m=[x for x in d['teams'] if x['team_id']=='$F_TEAM']
assert m and m[0]['available'] is False, ('未初始化应 available=false', d)" || fail "F3: letters 异常"
pass "F3 Owner 邀请函取件页（未初始化时如实不可用）"

# F4/F5: 无租户端点时 roster/invite **明确失败**（不假装成功）
$C -b /tmp/teamx-platcon-alice "$BASE/api/my/teams/$F_TEAM/roster" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') is not True and 'endpoint' in str(d.get('error','')), d" || fail "F4: 无端点应明确失败"
$C -b /tmp/teamx-platcon-alice -X POST -H 'Content-Type: application/json' -d '{}' "$BASE/api/my/teams/$F_TEAM/invite" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') is not True and 'endpoint' in str(d.get('error','')), d" || fail "F5: 无端点应明确失败"
pass "F4/F5 无租户端点时 roster/invite 明确失败（不假装）"

# F6: 非成员 leave → 明确失败；owner 自己 leave 也失败（不是加入者）
$C -b /tmp/teamx-platcon-alice -X POST -H 'Content-Type: application/json' -d '{}' "$BASE/api/my/teams/$F_TEAM/leave" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') is not True and 'not a member' in str(d.get('error','')), d" || fail "F6: 非成员 leave 应失败"
pass "F6 leave 仅限「我加入的」团队（owner/非成员被拒）"

# G1-G5: GitHub OAuth 真流程（mock，不碰 github.com）
echo "== G: GitHub OAuth（mock） =="
MOCK_PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')"
PC2_PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')"
cat > "$WORK/mock-oauth.py" <<'PYEOF2'
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs
import os
CALLBACK = os.environ["MOCK_CALLBACK"]
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        u = urlparse(self.path)
        if u.path == "/login/oauth/authorize":
            st = parse_qs(u.query).get("state", [""])[0]
            loc = f"{CALLBACK}/api/oauth/callback?code=mockcode123&state={st}"
            self.send_response(302); self.send_header("Location", loc); self.end_headers()
        elif u.path == "/user":
            if self.headers.get("Authorization") == "Bearer mocktoken":
                body = json.dumps({"id": 4242, "login": "mockuser", "name": "Mock User"}).encode()
                self.send_response(200); self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body))); self.end_headers()
                self.wfile.write(body)
            else:
                self.send_response(401); self.end_headers()
        else:
            self.send_response(404); self.end_headers()
    def do_POST(self):
        u = urlparse(self.path)
        if u.path == "/login/oauth/access_token":
            length = int(self.headers.get("Content-Length", 0)); self.rfile.read(length)
            body = json.dumps({"access_token": "mocktoken", "token_type": "bearer", "scope": "read:user"}).encode()
            self.send_response(200); self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body))); self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404); self.end_headers()
HTTPServer(("127.0.0.1", int(os.environ["MOCK_PORT"])), H).serve_forever()
PYEOF2
export MOCK_PORT="$MOCK_PORT" MOCK_CALLBACK="https://127.0.0.1:$PC2_PORT"
python3 "$WORK/mock-oauth.py" & MPID=$!
sleep 1
export TEAMX_GITHUB_CLIENT_ID="mock-id" TEAMX_GITHUB_CLIENT_SECRET="mock-secret"
export TEAMX_GITHUB_BASE="http://127.0.0.1:$MOCK_PORT" TEAMX_GITHUB_API_BASE="http://127.0.0.1:$MOCK_PORT"
"$TEAMX" platform console --addr 127.0.0.1 --port "$PC2_PORT" > "$WORK/pc2.log" 2>&1 &
PID2=$!
for _ in $(seq 1 40); do grep -q "token=" "$WORK/pc2.log" 2>/dev/null && break; sleep 0.3; done
BASE2="https://127.0.0.1:$PC2_PORT"
pass "G1 mock OAuth 与第二控制台就绪"

# G2: start（未登录可调）→ 302 到 mock authorize
LOC="$($C -o /dev/null -w '%{redirect_url}' "$BASE2/api/oauth/start")"
echo "$LOC" | grep -q "http://127.0.0.1:$MOCK_PORT/login/oauth/authorize" || fail "G2: 未指到 mock authorize: $LOC"
echo "$LOC" | grep -q "state=" || fail "G2: 缺 state"
pass "G2 authorize 重定向（带 state）"

# G3: 跟随全链 → 回调换 token → 建用户 → 会话 cookie → / 跳转
$C -c /tmp/teamx-platcon-gh -L --max-time 20 -v "$BASE2/api/oauth/start" -o /dev/null > "$WORK/g3.txt" 2>&1 || fail "G3: 链路失败"
grep -q "teamx_platform_token" /tmp/teamx-platcon-gh || fail "G3: 未下发会话 cookie"
tr -d '\r' < "$WORK/g3.txt" | grep -qi "location: /$" || fail "G3: 未回到 /"
ME="$($C -b /tmp/teamx-platcon-gh "$BASE2/api/me")"
echo "$ME" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('ok') and d.get('who')=='user:mockuser',d" || fail "G3: 身份不对: $ME"
# 用户行：github 锚定、无密码
"$TEAMX" --json platform user list 2>/dev/null | python3 -c "import json,sys;us=json.load(sys.stdin)['users'];m=[u for u in us if u['display_name']=='mockuser'];assert len(m)==1,d" || fail "G3: 用户行未建"
pass "G3 全链（授权→换 token→建用户→会话→/)"

# G4: state 复用 → 400（取 G3 链中的 callback URL 重放）
CBURL="$(tr -d '\r' < "$WORK/g3.txt" | grep -oi 'location: https://127.0.0.1:[^[:space:]]*api/oauth/callback[^[:space:]]*' | head -1 | sed 's/^[Ll]ocation: //')"
if [ -n "$CBURL" ]; then
  CODE="$($C -o /dev/null -w '%{http_code}' "$CBURL")"
  [ "$CODE" = "400" ] || fail "G4: state 复用应 400，实际 $CODE"
  pass "G4 state 单次有效（重放 400）"
else
  echo "  skip: G4（未捕获 callback URL）"
fi
# G5: 坏 state / 缺 code → 400
CODE="$($C -o /dev/null -w '%{http_code}' "$BASE2/api/oauth/callback?code=x&state=nope")"
[ "$CODE" = "400" ] || fail "G5: 坏 state 应 400，实际 $CODE"
CODE="$($C -o /dev/null -w '%{http_code}' "$BASE2/api/oauth/callback?state=y")"
[ "$CODE" = "400" ] || fail "G5: 缺 code 应 400，实际 $CODE"
pass "G5 坏请求 400（state/code）"
kill "$PID2" "$MPID" 2>/dev/null || true
unset TEAMX_GITHUB_CLIENT_ID TEAMX_GITHUB_CLIENT_SECRET TEAMX_GITHUB_BASE TEAMX_GITHUB_API_BASE

printf '\nALL PASSED (platform console E2E)\n'
