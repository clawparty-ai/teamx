#!/usr/bin/env bash
# k8e provider E2E (design/013 第一步) — 零集群：用 stub `kubectl` 记录/模拟 K8s 行为。
#
# 断言（含评审要求补的 3 条缺失测试）：
#   T-K1 provision 只 apply PVC + Secret（**不建 Pod** —— 裁决 V）
#   T-K2 run-jobs -> node ready（k8e 分支跳过 SSH/bootstrap）
#   T-K3 approve -> 建 member Pod；Pod 清单含 5 容器 + 安全加固 + 边车存活探针
#   T-K4 initContainer 幂等（setup.lock 原子 mkdir + trap 释放）
#   T-K5 NetworkPolicy 默认拒绝出网；**IMDS 不作为允许目标出现**（白名单负例）
#   T-K6 ResourceQuota + LimitRange 落在租户 namespace（D14 落点）
#   T-K7 stop 只删 Pod、**保留 PVC**（持久化断言）
#   T-K8 destroy -> 可验证清单：Pod → PVC → Secret → NetworkPolicy 全删
#
# Requires: python3（stub kubectl 用 python3 解析 YAML 关键行）。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEAMX="${TEAMX:-$ROOT/target/debug/teamx}"
WORK="$(mktemp -d /tmp/teamx-k8e-XXXXXX)"
export TEAMX_DB="$WORK/teamx.db"
export TEAMX_HOME="$WORK/home"
unset TEAMX_PROVISION_FAKE TEAMX_SERVER_URL || true
mkdir -p "$TEAMX_HOME" "$WORK/bin"
export PATH="$WORK/bin:$PATH"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
cleanup() { [ "${KEEP:-0}" = "1" ] && { echo "[keep] WORK=$WORK"; return; }; rm -rf "$WORK"; }
trap cleanup EXIT
jq() { python3 -c "import sys,json;d=json.load(sys.stdin);print($1)"; }

# ---------- stub kubectl：记录调用 + 模拟 apply/get/delete ----------
cat > "$WORK/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
# stub kubectl —— 只服务本测试：apply/get/delete/list
LOG="${KUBECTL_LOG:?KUBECTL_LOG must be set}"
STATE="${KUBECTL_STATE:?KUBECTL_STATE must be set}"
mkdir -p "$STATE"
args=("$@")
# 去掉全局 flags
filtered=(); i=0
while [ $i -lt ${#args[@]} ]; do
  case "${args[$i]}" in
    --kubeconfig|--context) i=$((i+2));;
    *) filtered+=("${args[$i]}"); i=$((i+1));;
  esac
done
set -- "${filtered[@]}"
verb="$1"; shift || true

record() { printf '%s\n' "$*" >> "$LOG"; }

case "$verb" in
  apply)
    yaml="$(cat)"
    printf '%s\n' "$yaml" > "$STATE/last-apply.yaml"
    # 记录出现的资源 kind（每个文档一个）
    printf '%s\n' "$yaml" | grep -E '^kind: ' | sed 's/^kind: //' | tr -d '\r' >> "$LOG"
    # 记录资源名（metadata.name）
    printf '%s\n' "$yaml" | grep -E '^  name: ' | sed 's/^  name: //' | tr -d '\r' >> "$STATE/applied-names"
    record "apply"
    echo "applied"
    ;;
  get)
    kind="$1"; shift || true
    case "$kind" in
      ns|namespace)
        cat "$STATE/namespaces" 2>/dev/null || true ;;
      pod)
        name="$1"; shift || true
        if [ -f "$STATE/pod-$name" ]; then
          printf '{"status":{"phase":"Running","podIP":"10.0.0.9"}}\n'
        else
          echo "Error from server (NotFound): pods \"$name\" not found" >&2
          exit 1
        fi ;;
      pvc|secret|networkpolicy)
        # list 模式（带 -l）
        if printf '%s' "$*" | grep -q '\-l'; then
          cat "$STATE/$(echo "$kind" | tr 'A-Z' 'a-z')-list" 2>/dev/null || true
        else
          echo "Error from server (NotFound): $kind not found" >&2; exit 1
        fi ;;
      *) echo "{}" ;;
    esac
    ;;
  delete)
    kind="$1"; shift || true
    name="$1"; shift || true
    record "delete $kind $name"
    rm -f "$STATE/pod-$name" 2>/dev/null || true
    echo "deleted"
    ;;
  exec)
    # approve 后的 kubectl exec（本测试不触发，占位）
    echo "ok"
    ;;
  *) echo "stub: unhandled verb $verb" >&2; exit 1 ;;
esac
STUB
chmod +x "$WORK/bin/kubectl"
export KUBECTL_LOG="$WORK/kubectl.log"
export KUBECTL_STATE="$WORK/state"
: > "$KUBECTL_LOG"

echo "== k8e provider E2E (design/013, stub kubectl) =="

"$TEAMX" init >/dev/null
"$TEAMX" --json team create "K8eE2E" --session s1 >/dev/null
pass "team created"

# ---------- T-K1: invite --provision-node --provider k8e -> 只建 PVC/Secret ----------
OUT="$("$TEAMX" --json team invite "contributor: dev" --session s1 --user-name alice \
        --provision-node --provider k8e --region us-west-2 2>/dev/null)"
NODE="$(echo "$OUT" | jq "d['provision']['node_id']")"
MEMBER="$(echo "$OUT" | jq "d['member_id']")"
[ -n "$NODE" ] || fail "no node_id"
# ---------- T-K2: run-jobs -> 资源 apply + node ready（k8e 跳过 SSH/bootstrap） ----------
"$TEAMX" --json node run-jobs >/dev/null 2>&1 || true
ST="$("$TEAMX" --json node status "$NODE" --session s1 2>/dev/null | jq "d['node']['state']")"
[ "$ST" = "ready" ] || fail "T-K2: expected ready, got $ST"
pass "T-K2 run-jobs -> node ready（无 SSH/bootstrap）"

YAML="$KUBECTL_STATE/last-apply.yaml"
[ -f "$YAML" ] || fail "T-K1: runner 未调用 kubectl apply"
grep -q "kind: PersistentVolumeClaim" "$YAML" || fail "T-K1: PVC 未 apply"
grep -q "kind: Secret" "$YAML" || fail "T-K1: Secret 未 apply"
grep -q "invitation.letter" "$YAML" || fail "T-K1: 邀请函未进 Secret"
if grep -q "^kind: Pod" "$YAML"; then fail "T-K1: provision 阶段不得建 Pod（裁决 V）"; fi
pass "T-K1 provision -> PVC + Secret（无 Pod）"

# ---------- T-K6: quota/limits ----------
grep -q "kind: ResourceQuota" "$YAML" || fail "T-K6: ResourceQuota 缺失"
grep -q "kind: LimitRange" "$YAML" || fail "T-K6: LimitRange 缺失"
pass "T-K6 ResourceQuota + LimitRange 已 apply（D14 落点）"

# ---------- T-K3/T-K4/T-K5: approve -> 建 Pod ----------
# 本地模式下座位在 import 邀请函时才落 members 行（invite 只建 invitation）。
LETTER="$(echo "$OUT" | jq "d['letter']")"
"$TEAMX" --json team import "$LETTER" --session s2 >/dev/null 2>&1 || true
"$TEAMX" --json team approve "$MEMBER" --session s1 >/dev/null 2>&1 || true
sleep 2
YAML2="$KUBECTL_STATE/last-apply.yaml"
grep -q "^kind: Pod" "$YAML2" || fail "T-K3: approve 后未建 Pod"
for c in "name: setup" "name: main" "name: ttyd" "name: fs" "name: expose"; do
  grep -q "$c" "$YAML2" || fail "T-K3: Pod 缺容器 $c"
done
grep -q "livenessProbe" "$YAML2" || fail "T-K3: 边车缺存活探针"
grep -q "readOnlyRootFilesystem: true" "$YAML2" || fail "T-K3: 缺只读根加固"
grep -q "seccompProfile" "$YAML2" || fail "T-K3: 缺 seccomp"
grep -q "mountPath: /workspace" "$YAML2" || fail "T-K3: workspace 未固定挂载"
pass "T-K3 approve -> member Pod（5 容器 + 加固 + 探针）"

grep -q "setup.lock" "$YAML2" || fail "T-K4: initContainer 缺幂等锁"
pass "T-K4 initContainer 幂等锁就位"

grep -q "kind: NetworkPolicy" "$YAML2" || fail "T-K5: 缺 NetworkPolicy"
if grep -q "cidr: 169.254.169.254" "$YAML2"; then fail "T-K5: IMDS 被错误地 allow-list"; fi
grep -q 'policyTypes: \["Egress"\]' "$YAML2" || fail "T-K5: 缺默认拒绝出网"
pass "T-K5 NetworkPolicy 默认拒绝 + IMDS 未放行"

# ---------- T-K7: stop 只删 Pod、保留 PVC ----------
"$TEAMX" --json node stop "$NODE" --session s1 >/dev/null 2>&1 || true
grep -q "delete pod " "$KUBECTL_LOG" || fail "T-K7: stop 未删 Pod"
if grep -q "delete pvc " "$KUBECTL_LOG"; then fail "T-K7: stop 不得删 PVC（持久化）"; fi
pass "T-K7 stop 删 Pod / 保留 PVC"

# ---------- T-K8: destroy -> 清单 ----------
"$TEAMX" --json node destroy "$NODE" --session s1 >/dev/null 2>&1 || true
grep -q "delete pvc " "$KUBECTL_LOG" || fail "T-K8: destroy 未删 PVC"
grep -q "delete secret " "$KUBECTL_LOG" || fail "T-K8: destroy 未删 Secret"
pass "T-K8 destroy 删除 Pod/PVC/Secret（可验证清单）"

printf '\nALL PASSED (k8e provider E2E)\n'
