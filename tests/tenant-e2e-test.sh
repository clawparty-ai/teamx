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
    # 租户 CA Secret：存成 JSON，供换发（reissue-members）读回
    if printf '%s\n' "$y" | grep -q 'ca.crt: |'; then
      printf '%s\n' "$y" | python3 -c '
import sys, json, base64, re
y = sys.stdin.read()
name = re.search(r"name: (teamx-ca-\S+)", y).group(1)
def grab(key):
    m = re.search(key + r": \|\n((?:    .*\n|\n)+)", y)
    if not m: return ""
    lines = [l[4:] if l.startswith("    ") else l for l in m.group(1).splitlines(True)]
    return "".join(lines)
data = {k: base64.b64encode(grab(k).encode()).decode() for k in ("ca.crt", "ca.key")}
json.dump({"data": data}, open(sys.argv[1], "w"))
' "$STATE/ca-secret.json"
    fi
    echo applied;;
  delete)
    kind="$1"; shift || true; name="$1"; shift || true
    echo "delete $kind $name" >> "$LOG"; echo deleted;;
  get)
    kind="$1"; shift || true
    case "$kind" in
      secret)
        # 租户 CA Secret 回放；其它 Secret 视为不存在
        name="$1"; shift || true
        if [ -f "$STATE/ca-secret.json" ] && printf '%s' "$name" | grep -q '^teamx-ca-'; then
          cat "$STATE/ca-secret.json"
        else
          echo "Error from server (NotFound): secrets \"$name\" not found" >&2; exit 1
        fi;;
      deployment,pvc,networkpolicy) echo "";;
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
cp "$KUBECTL_STATE/last-apply.yaml" "$WORK/tenant-manifests.yaml"
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

# N8: 成员证书换发（design/014 §8 ①②③）
: > "$KUBECTL_LOG"
R="$("$TEAMX" --json tenant reissue-members --team "$TEAM_ID" 2>&1)" || fail "N8: 换发失败: $R"
[ "$(echo "$R" | jq "d['ok'] and d['members'] >= 1")" = "True" ] || fail "N8: 无成员被换发"
[ "$(echo "$R" | jq "d['steps'][0]['done']")" = "True" ] || fail "N8: 步骤未完成: $R"
[ "$(echo "$R" | jq "d['steps'][0]['secret_applied'] and d['steps'][0]['server_url_switched']")" = "True" ] || fail "N8: 投递/切换未同时完成"
# 换发信确实投递为 Secret
grep -q "kind: Secret" "$KUBECTL_STATE/last-apply.yaml" || fail "N8: 未 apply 换发 Secret"
grep -q "teamx.io/kind: rotation-letter" "$KUBECTL_STATE/last-apply.yaml" || fail "N8: Secret 缺 rotation-letter 标签"
grep -q "teamx-inv:v1:" "$KUBECTL_STATE/last-apply.yaml" || fail "N8: Secret 内无换发信"
# server_url 同批切到租户 server（集群内 Service DNS）
TURL="$(echo "$R" | jq "d['server_url']")"
echo "$TURL" | grep -q "\.svc:" || fail "N8: server_url 不是租户 server: $TURL"
DBURL="$(sqlite3 "$TEAMX_DB" "SELECT server_url FROM nodes WHERE team_id='$TEAM_ID' AND state<>'terminated' LIMIT 1;")"
[ "$DBURL" = "$TURL" ] || fail "N8: nodes.server_url 未同批切换（$DBURL != ${TURL}）"
# 换发信由**租户 CA**签发：解出证书，验证 issuer 与租户 CA 一致 + CA 指纹 == 租户 CA 指纹
TFP="$(echo "$T1" | jq "d['tenant']['ca_fingerprint']")"
# 注意：ca_fingerprint_of 是 **DER** 的 SHA-256（不是 PEM 文本），用 openssl 转 DER 后再算
LFP="$(python3 - "$KUBECTL_STATE/last-apply.yaml" "$WORK" <<'PYEOF'
import base64, json, re, subprocess, sys, os
y = open(sys.argv[1]).read(); work = sys.argv[2]
m = re.search(r"letter: \|\n((?:    .*\n|\n)+)", y)
lines = [l[4:] if l.startswith("    ") else l for l in m.group(1).splitlines(True)]
payload = json.loads(base64.b64decode("".join(lines).strip().split("teamx-inv:v1:",1)[1]))
p = os.path.join(work, "letter-ca.pem"); open(p, "w").write(payload["certificates"]["ca_cert"])
der = subprocess.run(["openssl","x509","-in",p,"-outform","DER"],capture_output=True,check=True).stdout
import hashlib; print(hashlib.sha256(der).hexdigest())
PYEOF
)"
[ -n "$LFP" ] && [ "$LFP" = "$TFP" ] || fail "N8: 换发信 CA 指纹($LFP) != 租户 CA($TFP)"
python3 - "$KUBECTL_STATE/last-apply.yaml" <<'PYEOF' || fail "N8: 换发信证书链校验失败"
import base64, json, re, subprocess, sys, tempfile, os
y = open(sys.argv[1]).read()
m = re.search(r"letter: \|\n((?:    .*\n|\n)+)", y)
lines = [l[4:] if l.startswith("    ") else l for l in m.group(1).splitlines(True)]
letter = "".join(lines).strip()
payload = json.loads(base64.b64decode(letter.split("teamx-inv:v1:",1)[1]))
cert = payload["certificates"]["client_cert"]; ca = payload["certificates"]["ca_cert"]
d = tempfile.mkdtemp()
cp, cap = os.path.join(d,"c.pem"), os.path.join(d,"ca.pem")
open(cp,"w").write(cert); open(cap,"w").write(ca)
iss = subprocess.run(["openssl","x509","-in",cp,"-noout","-issuer"],capture_output=True,text=True).stdout
subj = subprocess.run(["openssl","x509","-in",cap,"-noout","-subject"],capture_output=True,text=True).stdout
iss_name = iss.split("issuer=",1)[1].strip(); ca_name = subj.split("subject=",1)[1].strip()
assert iss_name == ca_name, (iss_name, ca_name)
# 且与平台 CA 不同（租户 CA 独立）
subprocess.run(["openssl","verify","-CAfile",cap,cp],check=True,capture_output=True)
print("  issuer =", iss_name)
PYEOF
pass "N8 成员证书换发（租户 CA 签发 + Secret 投递 + server_url 同批切换）"

# N9: 租户 server 调度保护（weiye task-92 建议②）
# 注意：后续 apply（成员 Pod / 换发 Secret）会覆盖 last-apply.yaml，
# 因此用 N1 时保存的租户清单副本断言
Y1="$WORK/tenant-manifests.yaml"
[ -f "$Y1" ] || fail "N9: 缺租户清单副本"
grep -q "kind: PriorityClass" "$Y1" || fail "N9: 缺 PriorityClass"
grep -q "priorityClassName: teamx-tenant-server" "$Y1" || fail "N9: server Pod 未用 server 档"
grep -q "kind: PodDisruptionBudget" "$Y1" || fail "N9: 缺 PDB"
grep -q "maxUnavailable: 0" "$Y1" || fail "N9: PDB 未禁止自愿中断"
pass "N9 租户 server 调度保护（server 档 PriorityClass + PDB maxUnavailable=0）"

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
