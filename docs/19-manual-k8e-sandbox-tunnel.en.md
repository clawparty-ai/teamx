# Plan: Accessing Services Inside a k8e Sandbox from the dsh Host via teamx tunnel

> Applicable scenario: the user runs DeepSeek Harness (dsh) on their local machine
> (the dsh host), and uses the `dsh-k8e-sandbox` plugin (KIP-20) to route the
> agent's fs / subprocess / terminal into a cloud k8e sandbox (K8E Sandbox Matrix).
> Services started by the agent inside the sandbox (e.g.
> `python -m http.server 8080`, FastAPI, vLLM, etc.) listen on the sandbox pod's
> internal network and cannot be accessed directly from the local machine. This plan
> uses a teamx reverse tunnel so that the dsh host (local) can reach services inside
> the sandbox pod over the public internet.

## 1. Goals

- Let the **dsh host (local)** access services **inside the cloud sandbox pod** via HTTP/TCP.
- Use the teamx reverse tunnel to carry traffic across the public internet, in `local` mode (default, team members only).
- Clarify the inbound network policy constraints of the k8e sandbox, and provide complete, reproducible operating steps.

## 2. Topology

```
┌─ Cloud ─────────────────────────────────────────────────────────────┐
│                                                                     │
│  [Sandbox pod (gVisor) : service :8080]                             │
│        │  (pod network; CNP must allow 8080 inbound)                │
│  [k8e server/worker node]                                           │
│      ├─ kubectl port-forward pod/<pod> 18080:8080 → 127.0.0.1:18080 │
│      └─ teamx tunnel expose svc --port 18080 (local mode)           │
│           │ persistent mTLS WebSocket (node → cloud)                │
│  [teamx serve (cloud VM, public IP, :5781)] ── tunnel registry + relay │
└────────────┬────────────────────────────────────────────────────────┘
             │ public HTTPS/WSS relay
┌────────────▼────────────────────────────────────────────────────────┐
│  [dsh host (local)]                                                 │
│      └─ teamx tunnel forward svc --local-port 18080                 │
│           └─ curl http://127.0.0.1:18080/  → reaches the sandbox service │
└─────────────────────────────────────────────────────────────────────┘
```

Data flow (local mode, everything goes through the serve relay):

```
dsh host curl 127.0.0.1:18080
  → teamx forward local listener
  → public HTTPS → teamx serve (relay bridge)
  → persistent WebSocket of the node's teamx expose
  → node 127.0.0.1:18080 (kubectl port-forward)
  → sandbox pod :8080
```

## 3. Prerequisites

| Item | Requirement | Notes |
|---|---|---|
| teamx serve | Running on a cloud VM, publicly reachable | `~/.teamx/serve.json` records the URL, e.g. `https://<public IP>:5781` |
| teamx CLI | Installed on both the cloud node and the dsh host | `~/ .local/bin/teamx` (installed by `install.sh`) |
| Team | One network-mode team | owner creates the team + `serve start` |
| Membership | The cloud node and the dsh host are both members of **the same team** | mTLS certificates (invitation letter) are the identity |
| k8e cluster | Running in the cloud, nodes loggable into | Has `kubectl` + kubeconfig |
| Sandbox session | The dsh session is already routed into the sandbox | Visible as Active via `k8e-sandbox-cli sessions` |

## 4. Core Rationale (why this must be done this way)

1. **The sandbox pod denies all non-sandboxd inbound traffic by default**. Each session generates
   `CiliumNetworkPolicy sandbox-session-<sid>`
   (`buildSessionCNP` in `k8e/pkg/sandboxmatrix/grpc/orchestrator.go`);
   ingress allows only:
   - `fromEntities: [host]` → port `2024`
   - `fromEndpoints: [app=sandbox-grpc-gateway]` → port `2024`
   - `fromEndpoints: [app=e2b-server]` → port `2024`

   Once a CNP selects an endpoint, all unmatched inbound traffic is dropped. Therefore an 8080 service
   started by the agent in the pod is **unreachable by anyone (including the node, including
   kubectl port-forward)** — you must first explicitly append an ingress rule allowing the service port.

2. **The sandbox gateway has no port-forward / TCP proxy RPC**. `SandboxService`
   (`k8e/proto/sandbox/v1/sandbox.proto`) only has fs / exec / pty style interfaces; it cannot pass through
   arbitrary TCP ports. So we must detour via `kubectl port-forward` to bridge the pod port to node-local.

3. **Egress is relatively permissive**: the pod by default allows DNS 53 + TCP 443 to world. This means "running the tunnel provider inside the pod" (Plan B, see §10) is feasible but restricted to 443; this plan uses the
   node provider instead, avoiding that limitation.

4. **teamx tunnel is a reverse tunnel**: the provider exposes "its own local port" via the serve relay;
   the consumer maps it locally with `forward`. In a public-internet topology, `same_subnet` direct connection does not apply;
   everything goes through the relay.

## 5. Step-by-Step Operations

### 5.1 Team and membership

**owner (create team + serve, if not yet done)**:

```bash
teamx team create sandbox-access
teamx serve start                       # on the cloud VM, bound to a publicly accessible address/port
# Record the serve URL: https://<public IP>:5781
```

**Cloud k8e node joins the team** (either way):

```bash
# Method 1: join directly via CLI (owner approves)
teamx team join <invite_token> k8e-node --server https://<public IP>:5781

# Method 2: place letter certificates (after the owner issues teamx_team_invite)
mkdir -p ~/.teamx/letters/<letter-id>
cp client.crt client.key ca.crt ~/.teamx/letters/<letter-id>/
```

Once the owner approves with `teamx approve <member_id>`, the node holds an mTLS identity.

**dsh host**: already a team member (from earlier steps), certificate at `~/.teamx/letters/<id>/`.

### 5.2 Confirm the sandbox session and pod

```bash
# On the dsh host or any machine with permission
k8e-sandbox-cli sessions                # get the SID

# On the cloud node
kubectl -n sandbox-matrix get pods -l sandbox.k8e.io/session-id=<SID> -o wide
kubectl -n sandbox-matrix get cnp sandbox-session-<SID>   # confirm the CNP
```

The in-sandbox service must bind `0.0.0.0` (e.g. run in the terminal:
`python3 -m http.server 8080 --bind 0.0.0.0`), otherwise nobody can get in.

### 5.3 Allow the service port inbound (critical)

```bash
SID=<sandbox session ID>
SVC_PORT=8080

kubectl -n sandbox-matrix patch cnp sandbox-session-$SID --type=json -p="[
  {\"op\":\"add\",\"path\":\"/spec/ingress/-\",
   \"value\":{\"fromEntities\":[\"host\"],
             \"toPorts\":[{\"ports\":[{\"port\":\"$SVC_PORT\",\"protocol\":\"TCP\"}]}]}}]"
```

Verify:

```bash
kubectl -n sandbox-matrix get cnp sandbox-session-$SID -o yaml | grep -A6 "fromEntities"
```

### 5.4 Bridge the pod port on the node

```bash
kubectl -n sandbox-matrix port-forward pod/<pod> 18080:8080
# keep it in the foreground; or in the background: nohup ... &
```

Verify (another terminal):

```bash
curl -s http://127.0.0.1:18080/    # should return the sandbox service content
```

### 5.5 Expose the tunnel on the node (provider)

```bash
export TEAMX_SERVER_URL=https://<public IP>:5781
export TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt
export TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key
export TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt

teamx tunnel expose svc --port 18080          # local mode (default)
# If the CLI insists on --session: pass that member's session key (see teamx member list)
```

Expected: registration succeeds, `mode: local` (serve exposes no port).

> Optional frp mode (public port, for direct access by non-team-members):
>
> ```bash
> teamx tunnel expose svc --port 18080 --mode frp
> # returns public_port, e.g. 9100 → anyone can access tcp://<public IP>:9100
> ```

### 5.6 Consume on the dsh host (consumer)

```bash
# dsh host (dsh-plugin V1 has no tunnel tools; use the CLI)
export TEAMX_SERVER_URL=https://<public IP>:5781
export TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt
export TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key
export TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt

teamx tunnel list                      # should list svc
teamx tunnel forward svc --local-port 18080
```

If opencode-plugin is installed on the machine, you can also directly:

```
/team tunnel forward svc --local-port 18080
```

### 5.7 Verify

```bash
curl -s http://127.0.0.1:18080/               # the sandbox service responds
teamx tunnel status svc                       # look at mode/relay_addr
```

Expected status:

```json
{
  "name": "svc",
  "port": 18080,
  "target_port": 8080,
  "lan_ip": "<node internal IP>",
  "same_subnet": false,
  "relay_addr": "https://<public IP>:5781"
}
```

## 6. Wrap-Up and Cleanup

```bash
teamx tunnel close svc        # release the tunnel
# stop the port-forward (Ctrl-C / kill)
kubectl -n sandbox-matrix delete cnp sandbox-session-$SID   # optional; CNP auto-cleaned when the session is destroyed
```

## 7. Automation Script (recommended)

Because sandbox sessions are ephemeral, §5.3–5.5 must be re-run per session; wrap it in a script:

```bash
#!/usr/bin/env bash
# expose-sandbox <sid> <svc_port> <tunnel_name> [local_port]
set -euo pipefail
SID=$1; SVC_PORT=$2; NAME=$3; LOCAL_PORT=${4:-$((RANDOM%1000+18000))}
POD=$(kubectl -n sandbox-matrix get pods -l sandbox.k8e.io/session-id=$SID -o jsonpath='{.items[0].metadata.name}')

# 1. Allow CNP ingress
kubectl -n sandbox-matrix patch cnp sandbox-session-$SID --type=json -p="[{\"op\":\"add\",\"path\":\"/spec/ingress/-\",\"value\":{\"fromEntities\":[\"host\"],\"toPorts\":[{\"ports\":[{\"port\":\"$SVC_PORT\",\"protocol\":\"TCP\"}]}]}}]"

# 2. port-forward
kubectl -n sandbox-matrix port-forward pod/$POD $LOCAL_PORT:$SVC_PORT &
PF_PID=$!
sleep 2

# 3. Expose the tunnel
TEAMX_SERVER_URL=https://<public IP>:5781 \
TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt \
TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key \
TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt \
  teamx tunnel expose $NAME --port $LOCAL_PORT

echo "consumer: teamx tunnel forward $NAME --local-port $LOCAL_PORT"
wait $PF_PID
```

## 8. Troubleshooting

| Symptom | Cause | Remedy |
|---|---|---|
| Access timeout/refused | CNP not allowing | §5.3 patch; confirm `ingress` contains the service port |
| Service unreachable but CNP allows | Service binds 127.0.0.1 | Change bind to 0.0.0.0 |
| `expose` reports requires network mode | `TEAMX_SERVER_URL` not set | Export it and retry |
| `expose` reports already exists | Leftover same-name tunnel | `tunnel close` or rename |
| `forward` can't connect | Wrong mTLS cert / member not approved | Check `~/.teamx/letters/<id>/` and `teamx status` |
| Public unreachable | Cloud security group not opening 5781 | Open it in the cloud console |
| Lost after the pod is recycled | Session destroyed | Recreate the session and rerun the script |

## 9. Risks and Limitations

- **CNP allowlisting is a hard prerequisite**; it is part of the sandbox isolation design and cannot be bypassed (except via Plan B).
- Tunnel lifetime ≤ sandbox session lifetime; pausing/recycling the pod breaks it.
- `local` mode is only accessible to team members of the same team; `frp` mode opens to the public internet — not recommended by default.
- Tunnels are TCP-level and support arbitrary protocols (HTTP/SSH/DB/custom).

## 10. Alternative Plan B: Run the Tunnel Provider Inside the Sandbox Pod

> Not recommended; recorded for comparison only. Core idea: use the pod's default-allowed outbound TCP 443 to have an in-pod process initiate the tunnel directly toward teamx serve, without changing CNP ingress and without making the node a member.

Constraints:

- teamx serve must be reachable at **TCP 443** (the default CNP only allows 53/443);
  set `TEAMX_SERVER_URL` to `https://<public IP>` (443).
- You must write the teamx binary + mTLS certificates into the sandbox
  (`k8e-sandbox-cli write <sid> /workspace/teamx ...` + `chmod +x`).
- Start the provider in the sandbox terminal: `... /workspace/teamx tunnel expose <name> --port <svc_port>`.
- The pod is ephemeral; tunnel lifetime = session lifetime.

Pros: doesn't touch the CNP and the node need not be a member. Cons: the server must be reachable on 443 (otherwise you must change the session egress policy or enable the Cilium DNS proxy + allowedHosts); teamx is a Rust binary that must be able to run in the sandbox image; heavier operations.

## 11. Related Source Locations (for reference)

- Sandbox CNP generation: `buildSessionCNP` in `k8e/pkg/sandboxmatrix/grpc/orchestrator.go`
- Namespace / labels: `sandbox-matrix`, `sandbox.k8e.io/session-id`
- Sandbox gateway RPC surface: `k8e/proto/sandbox/v1/sandbox.proto` (no port-forward)
- teamx tunnel mechanics: `teamx/docs/17-manual-tunnel.md`, `teamx/crates/teamx/src/tunnel_client.rs`
- dsh plugin notes: `teamx/dsh-plugin/README.md` (V1 has no tunnel tools)
