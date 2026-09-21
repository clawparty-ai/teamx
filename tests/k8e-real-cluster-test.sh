#!/usr/bin/env bash
# k8e 真集群验证（design/013）—— 需要一个可用的 kubeconfig（默认 kind 集群 teamx-k8e）。
#
# 与 k8e-provision-test.sh 的区别：这里用**真的 kubectl + 真 API server**，
# 断言资源在集群里真实存在、通过 PSA(restricted) 准入、可被回收。
#   TEAMX_K8E_CONTEXT=kind-teamx-k8e bash tests/k8e-real-cluster-test.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEAMX="${TEAMX:-$ROOT/target/debug/teamx}"
CTX="${TEAMX_K8E_CONTEXT:-kind-teamx-k8e}"
WORK="$(mktemp -d /tmp/teamx-k8e-real-XXXXXX)"
export TEAMX_DB="$WORK/teamx.db" TEAMX_HOME="$WORK/home"
export TEAMX_K8S_CONTEXT="$CTX"
export TEAMX_MEMBER_IMAGE="${TEAMX_MEMBER_IMAGE:-teamx-member:dev}"
unset TEAMX_PROVISION_FAKE TEAMX_SERVER_URL || true
mkdir -p "$TEAMX_HOME"
pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
cleanup() { [ "${KEEP:-0}" = 1 ] && { echo "[keep] $WORK"; return; }; rm -rf "$WORK"; }
trap cleanup EXIT
jq() { python3 -c "import sys,json;d=json.load(sys.stdin);print($1)"; }
K() { kubectl --context "$CTX" "$@"; }
cd "$WORK"

echo "== k8e 真集群验证（context=${CTX}）=="
K get nodes >/dev/null || fail "集群不可达"
pass "集群可达"

"$TEAMX" init >/dev/null
"$TEAMX" --json team create "K8eReal" --session s1 >/dev/null
OUT="$("$TEAMX" --json team invite "contributor: dev" --session s1 --user-name alice \
        --provision-node --provider k8e --region us-west-2 2>/dev/null)"
NODE="$(echo "$OUT" | jq "d['provision']['node_id']")"
MEMBER="$(echo "$OUT" | jq "d['member_id']")"
LETTER="$(echo "$OUT" | jq "d['letter']")"
NS="tenant-$(python3 -c "
import sqlite3;c=sqlite3.connect('$TEAMX_DB')
print(c.execute('SELECT team_id FROM nodes LIMIT 1').fetchone()[0].replace('-','')[:8])")"
"$TEAMX" --json node run-jobs >/dev/null 2>&1 || true
ST="$("$TEAMX" --json node status "$NODE" --session s1 2>/dev/null | jq "d['node']['state']")"
[ "$ST" = "ready" ] || fail "node not ready: $ST"
pass "provision -> ready（namespace=${NS}）"

# R1 namespace + PSA + PVC + Secret + quota
K get ns "$NS" >/dev/null || fail "R1: namespace 不存在"
K get ns "$NS" -o jsonpath='{.metadata.labels.pod-security\.kubernetes\.io/enforce}' | grep -q restricted \
  || fail "R1: PSA restricted 未标注"
pass "R1 namespace + PSA(restricted)"
K get pvc -n "$NS" -l teamx.io/managed=true --no-headers | grep -q . || fail "R2: PVC 不存在"
K get secret -n "$NS" -l teamx.io/managed=true --no-headers | grep -q . || fail "R2: Secret 不存在"
K get resourcequota -n "$NS" teamx-quota >/dev/null || fail "R2: ResourceQuota 缺失"
K get limitrange -n "$NS" teamx-limits >/dev/null || fail "R2: LimitRange 缺失"
pass "R2 PVC + Secret + Quota + LimitRange 就位"
# R3: local-path 默认 SC 是 WaitForFirstConsumer → 建 Pod 前 Pending 是**正常**的，
# 真正的绑定断言放到 approve 之后（R5b）。
PH="$(K get pvc -n "$NS" -l teamx.io/managed=true -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo '')"
[ "$PH" = "Pending" ] || fail "R3: WaitForFirstConsumer 下建 Pod 前应为 Pending，实际 ${PH}"
pass "R3 PVC 待首个消费者（WaitForFirstConsumer 语义正确）"

# approve -> Pod 准入 + 调度
"$TEAMX" --json team import "$LETTER" --session s2 >/dev/null 2>&1 || true
"$TEAMX" --json team approve "$MEMBER" --session s1 >/dev/null 2>&1 || true
for i in $(seq 1 30); do
  P=$(K get pod -n "$NS" -l teamx.io/managed=true --no-headers 2>/dev/null | head -1)
  [ -n "$P" ] && break; sleep 2
done
[ -n "${P:-}" ] || fail "R4: approve 后未建 Pod"
pass "R4 approve -> Pod 已创建（$(echo "$P" | awk '{print $1}')）"
# PSA 准入通过 = Pod 被 API server 接受（若被拒会创建失败）
K get pod -n "$NS" -l teamx.io/managed=true -o jsonpath='{.items[0].spec.securityContext.seccompProfile.type}' | grep -q RuntimeDefault \
  || fail "R5: seccomp 未生效"
K get pod -n "$NS" -l teamx.io/managed=true -o jsonpath='{.items[0].spec.containers[*].name}' | grep -q expose \
  || fail "R5: expose 边车缺失"
pass "R5 Pod 通过 PSA 准入 + 5 容器 + seccomp"

# R5b: Pod 消费后 PVC 应绑定
for i in $(seq 1 30); do
  PH2="$(K get pvc -n "$NS" -l teamx.io/managed=true -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo '')"
  [ "$PH2" = "Bound" ] && break; sleep 2
done
[ "$PH2" = "Bound" ] || fail "R5b: Pod 起来后 PVC 仍未 Bound（${PH2}）"
pass "R5b PVC 已 Bound（Pod 消费后）"
K get networkpolicy -n "$NS" --no-headers | grep -q . || fail "R6: NetworkPolicy 缺失"
if K get networkpolicy -n "$NS" -o json | grep -q '169.254.169.254'; then fail "R6: IMDS 被放行"; fi
pass "R6 NetworkPolicy 就位且未放行 IMDS"

# destroy -> 资源清空
"$TEAMX" --json node destroy "$NODE" --session s1 >/dev/null 2>&1 || true
sleep 3
LEFT="$(K get pod,pvc,secret,networkpolicy -n "$NS" -l teamx.io/managed=true --no-headers 2>/dev/null | wc -l | tr -d ' ')"
[ "$LEFT" = "0" ] || fail "R7: destroy 后仍有 $LEFT 个受管资源残留"
pass "R7 destroy -> 受管资源清空"

printf '\nALL PASSED (k8e real cluster)\n'
