#!/usr/bin/env bash
# 租户全链 E2E（design/014 第二步）——零集群：stub kubectl 记录/模拟 K8s。
#
# 覆盖：
#   N1 tenant create → namespace + 独立 CA + server（PVC/Deployment）
#   N2 租户 CA 独立于平台 CA（指纹不同、互不影响）
#   N3 幂等：重复 create 不换 CA（换 CA 会作废成员证书）
#   N4 成员链与租户**共存于同一 namespace**（invite --provision-node --provider k8e → run-jobs → approve）
#   N5 destroy → 可验证清单（inventory → delete-namespace → delete-tenant-row → revoke-tenant-ca）
#   N6 destroy 后 tenants 表清空；重复 destroy 报 not found
#   N7 命名满足 M5（namespace ≥16 字符）
#
# Requires: python3 + sqlite3。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEAMX="${TEAMX:-$ROOT/target/debug/teamx}"
WORK="$(mktemp -d /tmp/teamx-tenant-XXXXXX)"
export TEAMX_DB="$WORK/teamx.db"
export TEAMX_HOME="$WORK/home"
unset TEAMX_PROVISION_FAKE TEAMX_SERVER_URL || true
mkdir -p "$TEAMX_HOME" "$WORK/bin"
export PATH="$WORK/bin:$PATH"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
cleanup() { [ "${KEEP:-0}" = 1 ] && { echo "[keep] $WORK"; return; }; rm -rf "$WORK"; }
trap cleanup EXIT
jq() { python3 -c "import sys,json;d=json.load(sys.stdin);print($1)"; }

# stub kubectl（记录 + 模拟；与 k8e E2E 同风格）
cat > "$WORK/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
LOG="${KUBECTL_LOG:?}"; STATE="${KUBECTL_STATE:?}"; mkdir -p "$STATE"
args=(); i=0
while [ $i -lt ${#args[@]} ]; do :; done
for a in "$@"; do
  case "$a" in
    --kubeconfig|--context) skip=1; continue;;
  esac
  if [ "${skip:-0}" = "1" ]; then skip=0; continue; fi
  args+=("$a")
done
set -- "${args[@]}"
verb="$1"; shift || true
case "$verb" in
  apply)
    y="$(cat)"; printf '%s\n' "$y" > "$STATE/last-apply.yaml"
    printf '%s\n' "$y" | grep -E '^kind: ' | sed 's/^kind: //' >> "$LOG"
    echo applied;;
  delete)
    kind="$1"; shift || true; name="$1"; shift || true
    echo "delete $kind $name" >> "$LOG"; echo deleted;;
  get)
    kind="$1"; shift || true
    case "$kind" in
      deployment,pvc,secret,networkpolicy|deployment) echo "";;
      ns|namespace) cat "$STATE/namespaces" 2>/dev/null || true;;
      *) echo "{}";;
    esac;;
  *) echo "stub: unhandled $verb" >&2; exit 1;;
esac
STUB
chmod +x "$WORK/bin/kubectl"
export KUBECTL_LOG="$WORK/kubectl.log" KUBECTL_STATE="$WORK/state"
: > "$KUBECTL_LOG"
cd "$WORK"

echo "== 租户全链 E2E（design/014 第二步，stub kubectl）=="
"$TEAMX" init >/dev/null
"$TEAMX" --json team create "TenantE2E" --session s1 >/dev/null
TEAM_ID="$(sqlite3 "$TEAMX_DB" "SELECT id FROM teams LIMIT 1;")"
pass "team 就绪"

# N1: tenant create
T1="$("$TEAMX" --json tenant create --team "$TEAM_ID" --server-image teamx-server:dev 2>&1)" || fail "N1: create 失败: $T1"
Y="$KUBECTL_STATE/last-apply.yaml"
for k in "kind: Namespace" "kind: Secret" "kind: PersistentVolumeClaim" "kind: Deployment"; do
  grep -q "$k" "$Y" || fail "N1: 缺 $k"
done
grep -q "ca.key: |" "$Y" || fail "N1: CA key 未注入"
grep -q "server.crt: |" "$Y" || fail "N1: server 证书未注入"
grep -q "replicas: 1" "$Y" || fail "N1: server 应单副本（SQLite 单写者）"
pass "N1 tenant create → namespace + 独立 CA + server(PVC/Deployment)"

# N2: 租户 CA 独立（指纹与平台 CA 不同）
FP="$(echo "$T1" | jq "d['tenant']['ca_fingerprint']")"
PLATFORM_CA="$(sqlite3 "$TEAMX_DB" "SELECT COUNT(*) FROM platform_tokens;" 2>/dev/null || echo 0)"
[ ${#FP} -eq 64 ] || fail "N2: 租户 CA 指纹异常"
[ -f "$TEAMX_HOME/ca/ca.crt" ] && PLAT="$(openssl x509 -in "$TEAMX_HOME/ca/ca.crt" -noout -fingerprint -sha256 2>/dev/null | tr -d ':' | tr 'A-F' 'a-f' | sed 's/.*=//')" || PLAT=""
[ "$FP" != "$PLAT" ] || fail "N2: 租户 CA 与平台 CA 相同（破坏独立信任域）"
pass "N2 租户 CA 独立于平台 CA（${FP:0:12}…）"

# N3: 幂等
T2="$("$TEAMX" --json tenant create --team "$TEAM_ID" 2>&1)"
[ "$(echo "$T2" | jq "d['reused']")" = "True" ] || fail "N3: 应复用"
[ "$(echo "$T2" | jq "d['tenant']['ca_fingerprint']")" = "$FP" ] || fail "N3: 复用不得换 CA"
pass "N3 幂等（不换 CA）"

# N4: 成员链与租户共存
INV="$("$TEAMX" --json team invite "contributor: dev" --session s1 --user-name alice \
        --provision-node --provider k8e --region us-west-2 2>/dev/null)"
MID="$(echo "$INV" | jq "d['member_id']")"
LETTER="$(echo "$INV" | jq "d['letter']")"
NODE="$(echo "$INV" | jq "d['provision']['node_id']")"
"$TEAMX" --json node run-jobs >/dev/null 2>&1 || true
"$TEAMX" --json team import "$LETTER" --session s2 >/dev/null 2>&1 || true
"$TEAMX" --json team approve "$MID" --session s1 >/dev/null 2>&1 || true
Y2="$KUBECTL_STATE/last-apply.yaml"
grep -q "^kind: Pod" "$Y2" || fail "N4: 成员 Pod 未创建"
# 注意：这里要 grep **文件内容**（不是路径字符串）；并容忍 grep 无匹配（pipefail 下会 set -e 退出）
MNS="$(grep -m1 -oE 'namespace: tenant-[a-z0-9]+' "$Y2" | awk '{print $2}' || true)"
[ "$MNS" = "$(echo "$T1" | jq "d['tenant']['namespace']")" ] || fail "N4: 成员不在租户 namespace（${MNS}）"
pass "N4 成员 Pod 与租户 server 共存于 $MNS"

# N5/N6: destroy 清单 + 清空
: > "$KUBECTL_LOG"
D="$("$TEAMX" --json tenant destroy --team "$TEAM_ID" 2>&1)" || fail "N5: destroy 失败: $D"
for s in inventory delete-namespace delete-tenant-row revoke-tenant-ca; do
  echo "$D" | grep -q "\"step\": \"$s" || fail "N5: 清单缺 $s"
done
grep -q "delete namespace tenant-" "$KUBECTL_LOG" || fail "N5: 未删 namespace"
[ "$(sqlite3 "$TEAMX_DB" "SELECT COUNT(*) FROM tenants;")" = "0" ] || fail "N6: tenants 表未清空"
if "$TEAMX" --json tenant destroy --team "$TEAM_ID" >/dev/null 2>&1; then fail "N6: 重复 destroy 应报 not found"; fi
pass "N5/N6 destroy 清单完整 + 幂等（重复 destroy 报 not found）"

# N7: 命名满足 M5
NS="$(echo "$T1" | jq "d['tenant']['namespace']")"
echo "$NS" | grep -Eq '^tenant-[a-z0-9]{16,}$' || fail "N7: namespace 命名不满足 M5: $NS"
pass "N7 namespace 命名满足 M5（${NS}）"

printf '\nALL PASSED (tenant full-chain E2E)\n'
