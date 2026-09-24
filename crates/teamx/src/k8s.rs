//! k8e / Kubernetes provider（design/013 第一步：只容器化 member）。
//!
//! 设计要点（见 `docs/ENT-13-teamx-cloud-impl-design-step1.cn.md`）：
//! - 一个 (team, member) 座位 = **一个 Pod**（D1=B）；`installation_id ↔ team_id ↔ namespace` 1:1:1。
//! - server **不持任何节点私钥**：走 `kubectl`（kubeconfig），无 SSH。
//! - workspace 固定挂载 `/workspace`；member 上报的 `workspace` 字段是唯一事实源（D3）。
//! - **approve 后才建 Pod**（裁决 V）：provision 阶段只建 PVC + Secret。
//! - 安全默认档：PSA(restricted) + seccomp + 只读根 + deny-all egress + 白名单 + 封 IMDS（D17）。
//!
//! 与 EC2 路径共用 `CloudProvider` / `nodes` / `provision_jobs` / 状态机 / destroy 清单（C1/C16）。

use serde_json::Value;

/// 受管资源的标签（list/destroy 靠它定位）。
pub const LABEL_MANAGED: &str = "teamx.io/managed=true";

/// `kubectl` 客户端。与 `AwsCli` 同样的"shell out 到官方 CLI"策略，避免引入重型依赖。
#[derive(Debug, Clone, Default)]
pub struct Kubectl {
    pub kubeconfig: Option<String>,
    pub context: Option<String>,
}

impl Kubectl {
    pub fn new(kubeconfig: Option<String>, context: Option<String>) -> Self {
        Self { kubeconfig, context }
    }

    /// 从环境变量构造：`TEAMX_KUBECONFIG` / `TEAMX_K8S_CONTEXT`。
    pub fn from_env() -> Self {
        Self {
            kubeconfig: std::env::var("TEAMX_KUBECONFIG").ok().filter(|s| !s.trim().is_empty()),
            context: std::env::var("TEAMX_K8S_CONTEXT").ok().filter(|s| !s.trim().is_empty()),
        }
    }

    fn base_args(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(k) = &self.kubeconfig {
            v.push("--kubeconfig".into());
            v.push(k.clone());
        }
        if let Some(c) = &self.context {
            v.push("--context".into());
            v.push(c.clone());
        }
        v
    }

    /// 执行 `kubectl <args>`（无 stdin）。
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = std::process::Command::new("kubectl");
        cmd.args(self.base_args());
        cmd.args(args);
        let out = cmd.output().map_err(|e| format!("kubectl spawn failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "kubectl {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// 执行 `kubectl <args>` 并把 `stdin` 喂进去（用于 `apply -f -` / `exec -i`）。
    fn run_with_stdin(&self, args: &[&str], input: &str) -> Result<String, String> {
        use std::io::Write;
        let mut cmd = std::process::Command::new("kubectl");
        cmd.args(self.base_args());
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("kubectl spawn failed: {e}"))?;
        child
            .stdin
            .as_mut()
            .ok_or("kubectl stdin unavailable")?
            .write_all(input.as_bytes())
            .map_err(|e| format!("kubectl stdin write failed: {e}"))?;
        let out = child.wait_with_output().map_err(|e| format!("kubectl wait failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "kubectl {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// `kubectl apply -f -`（多文档 YAML）。
    pub fn apply(&self, yaml: &str) -> Result<String, String> {
        self.run_with_stdin(&["apply", "-f", "-"], yaml)
    }

    /// 删除一个资源；不存在视为成功（幂等）。
    /// 打/覆盖一个 label（`kubectl label <kind> <name> k=v --overwrite -n <ns>`）。
    pub fn label(&self, kind: &str, name: &str, key: &str, value: &str, namespace: &str) -> Result<(), String> {
        let kv = format!("{key}={value}");
        self.run(&["label", kind, name, &kv, "--overwrite", "-n", namespace]).map(|_| ())
    }

    pub fn delete(&self, kind: &str, name: &str, namespace: &str) -> Result<(), String> {
        let _ = self.run(&["delete", kind, name, "-n", namespace, "--ignore-not-found=true"]);
        Ok(())
    }

    /// 读一个资源为 JSON（不存在返回 `None`）。
    pub fn get_json(&self, kind: &str, name: &str, namespace: &str) -> Result<Option<Value>, String> {
        match self.run(&["get", kind, name, "-n", namespace, "-o", "json"]) {
            Ok(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| format!("bad kubectl json: {e}")),
            Err(e) if e.contains("NotFound") || e.contains("not found") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 按 label selector 列资源名。
    pub fn list_names(&self, kind: &str, namespace: &str, selector: &str) -> Result<Vec<String>, String> {
        let s = self.run(&[
            "get",
            kind,
            "-n",
            namespace,
            "-l",
            selector,
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}",
        ])?;
        Ok(s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
    }

    /// 在 Pod 内执行脚本（stdin 管道，`bash -s`）。
    pub fn exec_script(&self, namespace: &str, pod: &str, container: &str, script: &str) -> Result<String, String> {
        self.run_with_stdin(
            &["exec", "-i", pod, "-n", namespace, "-c", container, "--", "bash", "-s"],
            script,
        )
    }

    /// 列 namespace（用于隔离断言 / 排查）。
    pub fn list_namespaces(&self) -> Result<Vec<String>, String> {
        let s = self.run(&["get", "ns", "-o", "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}"])?;
        Ok(s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
    }
}

// ---------------------------------------------------------------------------
// 命名规范（含成员短 id，防同集群多租户/多设备碰撞 —— design/011 D6 子约束 2）
// ---------------------------------------------------------------------------

/// 资源 id 短标识：取 id 的字母数字前 **16** 位。
///
/// **M5 裁决（013 v3）**：namespace 是**安全边界**，8 字符截断（32bit 熵）碰撞 =
/// 跨租户混住，故要求**全长或 ≥16 字符 hash**。为防口径漂移，namespace 与
/// namespace 内的 Pod/PVC/Secret 统一用 16 字符（UUIDv4 前 16 个 hex = 64bit 熵）。
pub fn short_id(id: &str) -> String {
    let s: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).take(16).collect();
    if s.is_empty() { "unknown000000000".to_string() } else { s }
}

/// 租户 namespace：`tenant-<team 去横线前 16 字母数字>`（C11 三元组之一，M5）。
pub fn namespace_for(team_id: &str) -> String {
    format!("tenant-{}", short_id(team_id))
}

pub fn pod_name(member_id: &str) -> String {
    format!("teamx-m-{}", short_id(member_id))
}

pub fn pvc_name(member_id: &str) -> String {
    format!("teamx-ws-{}", short_id(member_id))
}

pub fn secret_name(member_id: &str) -> String {
    format!("teamx-letter-{}", short_id(member_id))
}

// ---------------------------------------------------------------------------
// 清单生成
// ---------------------------------------------------------------------------

/// member 容器镜像（`TEAMX_MEMBER_IMAGE` 可覆盖；默认按 tag 固定版本）。
pub fn member_image() -> String {
    std::env::var("TEAMX_MEMBER_IMAGE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "teamx-member:latest".to_string())
}

/// 默认资源规格（v1 不做配额，但每 Pod 固定 limit —— design/012 D14）。
pub fn default_resources() -> (String, String) {
    let req = std::env::var("TEAMX_MEMBER_CPU_REQ").unwrap_or_else(|_| "250m".into());
    let lim = std::env::var("TEAMX_MEMBER_CPU_LIMIT").unwrap_or_else(|_| "2".into());
    (req, lim)
}

/// 生成 namespace + PVC + Secret（provision 阶段，**不建 Pod** —— 裁决 V）。
pub fn provision_manifests(
    team_id: &str,
    member_id: &str,
    letter: &str,
    workspace_gib: u32,
    storage_class: Option<&str>,
) -> String {
    let ns = namespace_for(team_id);
    let pvc = pvc_name(member_id);
    let sec = secret_name(member_id);
    let sc = storage_class
        .map(|s| format!("  storageClassName: {s}\n"))
        .unwrap_or_default();
    format!(
        r#"apiVersion: v1
kind: Namespace
metadata:
  name: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/team: "{team_id}"
    pod-security.kubernetes.io/enforce: restricted
    pod-security.kubernetes.io/audit: restricted
    pod-security.kubernetes.io/warn: restricted
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: {pvc}
  namespace: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/team: "{team_id}"
    teamx.io/member: "{member_id}"
spec:
  accessModes: ["ReadWriteOnce"]
{sc}  resources:
    requests:
      storage: {workspace_gib}Gi
---
apiVersion: v1
kind: Secret
metadata:
  name: {sec}
  namespace: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/member: "{member_id}"
type: Opaque
stringData:
  invitation.letter: |
{letter_indented}
"#,
        letter_indented = letter
            .lines()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// 生成 member Pod（**approve 后**才 apply —— 裁决 V）。
///
/// 组成（design/013 §4）：initContainer `setup`（import letter + clone repo，幂等）
/// + `main`（opencode/worker）+ `ttyd` + `fs` + `expose` 边车。
#[allow(clippy::too_many_arguments)]
pub fn member_pod_manifest(
    team_id: &str,
    member_id: &str,
    server_url: &str,
    team_repo_url: &str,
    session: &str,
) -> String {
    let ns = namespace_for(team_id);
    let pod = pod_name(member_id);
    let pvc = pvc_name(member_id);
    let sec = secret_name(member_id);
    let rot = rotation_secret_name(member_id);
    let image = member_image();
    let (cpu_req, cpu_lim) = default_resources();
    format!(
        r#"apiVersion: v1
kind: Pod
metadata:
  name: {pod}
  namespace: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/team: "{team_id}"
    teamx.io/member: "{member_id}"
spec:
  restartPolicy: Always
  securityContext:
    runAsNonRoot: true
    runAsUser: 1000
    fsGroup: 1000
    seccompProfile: {{ type: RuntimeDefault }}
  initContainers:
    - name: setup
      image: {image}
      command: ["/bin/bash", "-lc"]
      args:
        - |
          set -euo pipefail
          mkdir -p /workspace/.teamx/letters/invite
          cp /etc/teamx/letter/invitation.letter /workspace/.teamx/letters/invite/invitation.letter
          chmod 600 /workspace/.teamx/letters/invite/invitation.letter
          # 幂等加锁：Pod 重试/并发时避免半途状态（评审必改 9）
          LOCK=/workspace/.teamx/setup.lock
          mkdir -p /workspace/.teamx
          if ! mkdir "$LOCK" 2>/dev/null; then
            echo "setup already running/done; skipping"
            exit 0
          fi
          trap 'rmdir "$LOCK" 2>/dev/null || true' EXIT
          # 幂等：已认领则跳过
          teamx team import /workspace/.teamx/letters/invite/invitation.letter --session {session} || true
          # 换发信（design/014 §8）：租户 CA 签发的成员证书 + 租户 server URL。
          # import 会把证书落到 .teamx/letters/<id>/，CLI 的 mTLS 发现按 host 精确匹配，
          # 因此切到租户 server 后会选到这一份（旧的平台 CA 信仍在但不匹配该 host）。
          if [ -f /etc/teamx/rotation/letter ]; then
            teamx team import "$(cat /etc/teamx/rotation/letter)" --session {session} || true
          fi
          # 幂等：已 clone 则只做 fetch
          if [ -d /workspace/.git ]; then
            git -C /workspace fetch --all --prune || true
          elif [ -n "{team_repo_url}" ]; then
            git clone {team_repo_url} /workspace || true
          fi
      env:
        - name: TEAMX_HOME
          value: /workspace/.teamx
        - name: TEAMX_SERVER_URL
          value: {server_url}
      volumeMounts:
        - {{ name: workspace, mountPath: /workspace }}
        - {{ name: letter, mountPath: /etc/teamx/letter, readOnly: true }}
        - {{ name: rotation, mountPath: /etc/teamx/rotation, readOnly: true }}
      securityContext:
        readOnlyRootFilesystem: true
        allowPrivilegeEscalation: false
        capabilities: {{ drop: ["ALL"] }}
  containers:
    - name: main
      image: {image}
      command: ["/bin/bash", "-lc"]
      args: ["teamx worker --interval 60 --cooldown 600 --opencode /usr/local/bin/opencode --opencode-url http://127.0.0.1:4201"]
      env: &teamx_env
        - name: TEAMX_HOME
          value: /workspace/.teamx
        - name: TEAMX_SERVER_URL
          value: {server_url}
        - name: TEAMX_WORKER_ENABLED
          value: "1"
      resources:
        requests: {{ cpu: "{cpu_req}", memory: "256Mi" }}
        limits: {{ cpu: "{cpu_lim}", memory: "4Gi" }}
      volumeMounts:
        - {{ name: workspace, mountPath: /workspace }}
        - {{ name: tmp, mountPath: /tmp }}
      securityContext:
        readOnlyRootFilesystem: true
        allowPrivilegeEscalation: false
        capabilities: {{ drop: ["ALL"] }}
    - name: ttyd
      image: {image}
      command: ["/bin/bash", "-lc"]
      args: ["ttyd -p 7681 -W -T xterm-256color /bin/bash -lc 'cd /workspace && exec bash'"]
      env: *teamx_env
      volumeMounts:
        - {{ name: workspace, mountPath: /workspace }}
        - {{ name: tmp, mountPath: /tmp }}
      resources:
        requests: {{ cpu: "50m", memory: "64Mi" }}
        limits: {{ cpu: "500m", memory: "512Mi" }}
      securityContext:
        readOnlyRootFilesystem: true
        allowPrivilegeEscalation: false
        capabilities: {{ drop: ["ALL"] }}
    - name: fs
      image: {image}
      command: ["/bin/bash", "-lc"]
      args: ["teamx fs --addr 127.0.0.1 --port 4197"]
      env: *teamx_env
      volumeMounts:
        - {{ name: workspace, mountPath: /workspace }}
        - {{ name: tmp, mountPath: /tmp }}
      resources:
        requests: {{ cpu: "50m", memory: "64Mi" }}
        limits: {{ cpu: "500m", memory: "512Mi" }}
      securityContext:
        readOnlyRootFilesystem: true
        allowPrivilegeEscalation: false
        capabilities: {{ drop: ["ALL"] }}
    - name: expose
      image: {image}
      command: ["/bin/bash", "-lc"]
      args:
        - |
          set -u
          teamx tunnel expose ttyd-{pod} --port 7681 --mode local --server {server_url} --session {session} &
          teamx tunnel expose fs-{pod} --port 4197 --mode local --server {server_url} --session {session} &
          wait
      env: *teamx_env
      volumeMounts:
        - {{ name: tmp, mountPath: /tmp }}
      livenessProbe:
        exec: {{ command: ["/bin/bash", "-lc", "pgrep -f 'tunnel expose' >/dev/null"] }}
        initialDelaySeconds: 20
        periodSeconds: 30
        failureThreshold: 3
      resources:
        requests: {{ cpu: "50m", memory: "64Mi" }}
        limits: {{ cpu: "500m", memory: "512Mi" }}
      securityContext:
        readOnlyRootFilesystem: true
        allowPrivilegeEscalation: false
        capabilities: {{ drop: ["ALL"] }}
  volumes:
    - name: workspace
      persistentVolumeClaim: {{ claimName: {pvc} }}
    - name: letter
      secret: {{ secretName: {sec} }}
    # 换发信（可选：只有做过证书换发才存在）
    - name: rotation
      secret:
        secretName: {rot}
        optional: true
    - name: tmp
      emptyDir: {{}}
"#
    )
}

/// namespace 级 ResourceQuota + LimitRange（design/012 D14：v1 不做计费，但"兜底"放在 K8s 层）。
pub fn quota_manifests(team_id: &str) -> String {
    let ns = namespace_for(team_id);
    format!(
        r#"apiVersion: v1
kind: ResourceQuota
metadata:
  name: teamx-quota
  namespace: {ns}
spec:
  hard:
    requests.cpu: "8"
    requests.memory: 16Gi
    limits.cpu: "16"
    limits.memory: 32Gi
    persistentvolumeclaims: "8"
---
apiVersion: v1
kind: LimitRange
metadata:
  name: teamx-limits
  namespace: {ns}
spec:
  limits:
    - type: Container
      default: {{ cpu: "1", memory: "2Gi" }}
      defaultRequest: {{ cpu: "250m", memory: "512Mi" }}
      max: {{ cpu: "4", memory: "8Gi" }}
"#
    )
}

/// 换发信 Secret 名（design/014 §8：租户 CA 签发的成员证书 + 租户 server URL）。
pub fn rotation_secret_name(member_id: &str) -> String {
    format!("teamx-rot-{}", short_id(member_id))
}

/// 生成换发信 Secret（放在租户 namespace 内；成员 Pod 的 setup 会读取并 import）。
pub fn rotation_secret_manifest(team_id: &str, member_id: &str, letter: &str) -> String {
    let ns = namespace_for(team_id);
    let name = rotation_secret_name(member_id);
    format!(
        r#"apiVersion: v1
kind: Secret
metadata:
  name: {name}
  namespace: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/team: "{team_id}"
    teamx.io/member: "{member_id}"
    teamx.io/kind: rotation-letter
type: Opaque
stringData:
  letter: |
{letter_indented}
"#,
        letter_indented = indent(letter, 4)
    )
}

/// 租户 server 镜像（`TEAMX_SERVER_IMAGE` 可覆盖）。
/// 租户 server 的 PriorityClass（013 M7 server 档；集群级、全租户共享）。
pub const SERVER_PRIORITY_CLASS: &str = "teamx-tenant-server";

pub fn server_image() -> String {
    std::env::var("TEAMX_SERVER_IMAGE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "teamx-server:latest".to_string())
}

/// 租户 server 的 Service/端口约定（与现网一致）。
pub const SERVER_PORT: u16 = 5783;

/// 租户基础设施清单（design/014 §3/§4）：namespace（PSA）+ CA Secret + server PVC + Deployment。
///
/// **CA 由平台在建租户时生成**并写入 Secret（租户 server 只读挂载；它需要 CA key 才能
/// 在 invite 时签发成员证书）。server 自身证书也由平台用该 CA 签好后放入同一 Secret。
pub fn tenant_manifests(
    team_id: &str,
    ca_cert_pem: &str,
    ca_key_pem: &str,
    server_cert_pem: &str,
    server_key_pem: &str,
) -> String {
    let ns = namespace_for(team_id);
    let ca_secret = format!("teamx-ca-{}", short_id(team_id));
    let pvc = format!("teamx-serve-data-{}", short_id(team_id));
    let deploy = format!("teamx-serve-{}", short_id(team_id));
    let image = server_image();
    format!(
        r#"apiVersion: scheduling.k8s.io/v1
kind: PriorityClass
metadata:
  name: {SERVER_PRIORITY_CLASS}
  labels:
    teamx.io/managed: "true"
    teamx.io/kind: tenant-server-priority
value: 100000
globalDefault: false
description: "teamx 租户 server（租户控制面，单副本）；013 M7 server 档。集群级共享，租户回收时不删除。"
---
apiVersion: v1
kind: Namespace
metadata:
  name: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/team: "{team_id}"
    teamx.io/kind: tenant
    pod-security.kubernetes.io/enforce: restricted
    pod-security.kubernetes.io/audit: restricted
    pod-security.kubernetes.io/warn: restricted
---
apiVersion: v1
kind: Secret
metadata:
  name: {ca_secret}
  namespace: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/team: "{team_id}"
    teamx.io/kind: tenant-ca
type: Opaque
stringData:
  ca.crt: |
{ca_crt}
  ca.key: |
{ca_key}
  server.crt: |
{srv_crt}
  server.key: |
{srv_key}
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: {pvc}
  namespace: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/team: "{team_id}"
spec:
  accessModes: ["ReadWriteOnce"]
  resources:
    requests:
      storage: 5Gi
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {deploy}
  namespace: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/team: "{team_id}"
    teamx.io/kind: tenant-server
spec:
  replicas: 1
  strategy: {{ type: Recreate }}
  selector:
    matchLabels:
      teamx.io/tenant-server: "{team_id}"
  template:
    metadata:
      labels:
        teamx.io/managed: "true"
        teamx.io/team: "{team_id}"
        teamx.io/tenant-server: "{team_id}"
    spec:
      priorityClassName: {SERVER_PRIORITY_CLASS}
      securityContext:
        runAsNonRoot: true
        runAsUser: 1000
        fsGroup: 1000
        seccompProfile: {{ type: RuntimeDefault }}
      containers:
        - name: serve
          image: {image}
          args: ["serve", "--addr", "0.0.0.0", "--port", "{SERVER_PORT}", "--db", "/data/teamx.db"]
          env:
            - name: TEAMX_HOME
              value: /data
            - name: TEAMX_DB
              value: /data/teamx.db
            - name: TEAMX_TENANT_TEAM_ID
              value: "{team_id}"
          ports:
            - name: mtls
              containerPort: {SERVER_PORT}
          # mTLS 下 kubelet 的 HTTPS 探针无客户端证书、握手必失败（真集群抓到 Pod 永远 0/1）。
          # server 在端口能连上后写 TEAMX_HEALTH_FILE（默认 /data/.teamx-ready），探针只读文件。
          readinessProbe:
            exec: {{ command: ["test", "-f", "/data/.teamx-ready"] }}
            initialDelaySeconds: 5
            periodSeconds: 10
          livenessProbe:
            exec: {{ command: ["test", "-f", "/data/.teamx-ready"] }}
            initialDelaySeconds: 20
            periodSeconds: 30
          resources:
            requests: {{ cpu: "100m", memory: "256Mi" }}
            limits: {{ cpu: "1", memory: "1Gi" }}
          volumeMounts:
            - {{ name: data, mountPath: /data }}
            - {{ name: ca, mountPath: /data/ca, readOnly: true }}
          securityContext:
            readOnlyRootFilesystem: true
            allowPrivilegeEscalation: false
            capabilities: {{ drop: ["ALL"] }}
      volumes:
        - name: data
          persistentVolumeClaim: {{ claimName: {pvc} }}
        - name: ca
          secret: {{ secretName: {ca_secret} }}
---
apiVersion: v1
kind: Service
metadata:
  name: {deploy}
  namespace: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/team: "{team_id}"
    teamx.io/kind: tenant-server-svc
spec:
  # 服务发现名与平台 service 车道拨号一致：
  # https://teamx-serve-<id>.<ns>.svc:<port>（design/014 §6.5）。
  # 注意：站外（operator 本机）的平台控制台直连不到 ClusterIP，
  # 需经 kube-apiserver proxy（开放项 O-5）。
  selector:
    teamx.io/tenant-server: "{team_id}"
  ports:
    - name: mtls
      port: {SERVER_PORT}
      targetPort: mtls
      protocol: TCP
---
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: {deploy}-pdb
  namespace: {ns}
  labels:
    teamx.io/managed: "true"
    teamx.io/team: "{team_id}"
    teamx.io/kind: tenant-server-pdb
spec:
  # 单副本控制面：自愿中断（节点 drain/升级）一律不得驱逐，须人工处理
  maxUnavailable: 0
  selector:
    matchLabels:
      teamx.io/tenant-server: "{team_id}"
"#,
        ca_crt = indent(ca_cert_pem, 4),
        ca_key = indent(ca_key_pem, 4),
        srv_crt = indent(server_cert_pem, 4),
        srv_key = indent(server_key_pem, 4),
    )
}

/// YAML 块缩进助手（`stringData` 的块标量）。
fn indent(s: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    s.lines().map(|l| format!("{pad}{l}")).collect::<Vec<_>>().join("\n")
}

/// 默认拒绝出网 + 显式白名单（NFR-1）。
///
/// **注意语义**：NetworkPolicy 是 **allow-list**（不列即拒绝），因此"封 IMDS"的
/// 正确做法是 **不把 `169.254.169.254` 放进白名单**（默认拒绝已覆盖），
/// 而不是写一条 allow 规则 —— 那样反而会放行。本函数只生成白名单；
/// `network_policy_blocks_imds` 单测断言清单里不出现 IMDS。
pub fn network_policy_manifest(team_id: &str, member_id: &str, allow_hosts: &[String]) -> String {
    let ns = namespace_for(team_id);
    let pod = pod_name(member_id);
    // 空白名单必须写成 `egress: []`（显式空列表 = 全部拒绝）；写成空值 `egress:`
    // 会被 API 当成 null 而拒（真集群踩到：NetworkPolicy 创建失败、静默缺失）。
    let mut egress = String::new();
    for h in allow_hosts {
        egress.push_str(&format!(
            "    - to:\n        - ipBlock:\n            cidr: {h}\n"
        ));
    }
    let egress_block = if egress.is_empty() {
        "  egress: []\n".to_string()
    } else {
        format!("  egress:\n{egress}")
    };
    format!(
        r#"apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {pod}-egress
  namespace: {ns}
spec:
  podSelector:
    matchLabels:
      teamx.io/member: "{member_id}"
  policyTypes: ["Egress"]
  # allow-list：仅放行显式列出的目标；其余（含 IMDS 169.254.169.254、租户间东西向）
  # 一律拒绝。IMDS 绝不能出现在下面的白名单里。
{egress_block}"#
    )
}

/// 把 member 上报的 workspace 路径规范化（D3：唯一事实源 = 容器内固定挂载点）。
pub const MEMBER_WORKSPACE: &str = "/workspace";


// ---------------------------------------------------------------------------
// CloudProvider 实现（design/013 §3）
// ---------------------------------------------------------------------------

use crate::provision::{CloudProvider, CreateReq, Created, InstanceIps};

/// k8e/K8s provider。`instance_id` = Pod 名；`ip` = Service/Pod 的集群内地址占位。
pub struct K8sProvider {
    kubectl: Kubectl,
    /// workspace 卷大小（GiB）
    pub workspace_gib: u32,
    /// 出网白名单（CIDR）；空 = 只允许集群内（默认拒绝其余，含 IMDS）
    pub egress_allow: Vec<String>,
}

impl K8sProvider {
    pub fn new(kubectl: Kubectl) -> Self {
        let egress_allow = std::env::var("TEAMX_K8S_EGRESS_ALLOW")
            .ok()
            .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default();
        Self { kubectl, workspace_gib: 30, egress_allow }
    }

    pub fn from_env() -> Self {
        Self::new(Kubectl::from_env())
    }

    /// provision 阶段：namespace + PVC + Secret（**不建 Pod**，裁决 V）。
    pub fn provision(&self, req: &CreateReq) -> Result<(), String> {
        let letter = req.letter.clone().unwrap_or_default();
        let team_id = req.team_id_hint();
        // 命名必须用 **member_id**（与 approve 阶段建 Pod 时一致），
        // 不能用 req.name（= teamx-<os_user>）—— 否则 Pod 引用的 PVC 不存在。
        let member_id = req.member_id_hint();
        let yaml = provision_manifests(&team_id, &member_id, &letter, self.workspace_gib, None);
        let quota = quota_manifests(&team_id);
        self.kubectl.apply(&format!("{yaml}---\n{quota}")).map(|_| ())
    }

    /// approve 阶段：建 member Pod（= 启动服务，裁决 V）+ NetworkPolicy。
    pub fn start_member(
        &self,
        team_id: &str,
        member_id: &str,
        server_url: &str,
        team_repo_url: &str,
        session: &str,
    ) -> Result<(), String> {
        let pod = member_pod_manifest(team_id, member_id, server_url, team_repo_url, session);
        let np = network_policy_manifest(team_id, member_id, &self.egress_allow);
        let combined = format!("{pod}---\n{np}");
        self.kubectl.apply(&combined).map(|_| ())
    }

    /// 停：删 Pod 保 PVC（对应 006 的 stop）。
    pub fn stop_member(&self, team_id: &str, member_id: &str) -> Result<(), String> {
        self.kubectl.delete("pod", &pod_name(member_id), &namespace_for(team_id))
    }

    /// 回收：Pod → PVC → Secret → Namespace（可验证清单，design/013 §3）。
    pub fn destroy_member(&self, team_id: &str, member_id: &str) -> Vec<serde_json::Value> {
        let ns = namespace_for(team_id);
        let mut steps = Vec::new();
        let pod = pod_name(member_id);
        let pvc = pvc_name(member_id);
        let sec = secret_name(member_id);
        for (step, kind, name) in [
            ("delete-pod", "pod", pod.clone()),
            ("delete-pvc", "pvc", pvc.clone()),
            ("delete-secret", "secret", sec.clone()),
        ] {
            let ok = self.kubectl.delete(kind, &name, &ns).is_ok();
            steps.push(serde_json::json!({ "step": step, "target": name, "done": ok }));
        }
        // NetworkPolicy
        let np = format!("{pod}-egress");
        let ok = self.kubectl.delete("networkpolicy", &np, &ns).is_ok();
        steps.push(serde_json::json!({ "step": "delete-networkpolicy", "target": np, "done": ok }));
        steps
    }
}

impl CloudProvider for K8sProvider {
    fn name(&self) -> &str {
        "k8e"
    }

    /// 建资源：namespace + PVC + Secret（Pod 留到 approve）。
    fn create(&self, req: &CreateReq) -> Result<Created, String> {
        self.provision(req)?;
        // instance_id 用 Pod 名（approve 时按同一名字创建，保持幂等）
        Ok(Created {
            instance_id: pod_name(&req.name),
            private_ip: Some(format!("{}.{}.svc", pod_name(&req.name), namespace_for(&req.team_id_hint()))),
            public_ip: None,
        })
    }

    /// 状态：Pod 不存在（还没 approve）视为 `ready`（资源就绪）；
    /// 存在则映射 Pod phase。
    fn status(&self, instance_id: &str) -> Result<String, String> {
        let (ns, pod) = split_instance(instance_id);
        match self.kubectl.get_json("pod", &pod, &ns)? {
            None => Ok("ready".to_string()),
            Some(v) => Ok(match v["status"]["phase"].as_str().unwrap_or("Pending") {
                "Running" => "running",
                // 成员是常驻服务：容器"正常退出"对成员语义就是**异常**（不应自己结束），
                // 映射为 failed 交给上层告警/重建（评审必改：Succeeded ≠ running）。
                "Succeeded" | "Failed" => "failed",
                _ => "provisioning",
            }
            .to_string()),
        }
    }

    fn destroy(&self, instance_id: &str) -> Result<(), String> {
        let (ns, pod) = split_instance(instance_id);
        self.kubectl.delete("pod", &pod, &ns)
    }

    fn list(&self) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        for ns in self.kubectl.list_namespaces()? {
            if !ns.starts_with("tenant-") {
                continue;
            }
            out.extend(self.kubectl.list_names("pod", &ns, LABEL_MANAGED)?);
        }
        Ok(out)
    }

    /// k8s 无 SSH 密钥（server 不持节点私钥）。
    fn delete_keypair(&self, _name: &str) -> Result<(), String> {
        Ok(())
    }

    fn stop(&self, instance_id: &str) -> Result<(), String> {
        let (ns, pod) = split_instance(instance_id);
        self.kubectl.delete("pod", &pod, &ns)
    }

    fn start(&self, _instance_id: &str) -> Result<(), String> {
        // 起 Pod 需要 member 上下文（letter/session），由 approve 流程负责；
        // 这里只报告"需要上层重建"。
        Err("k8e start: recreate the member Pod via approve/provision flow".to_string())
    }

    fn ip_of(&self, instance_id: &str) -> Result<Option<InstanceIps>, String> {
        let (ns, pod) = split_instance(instance_id);
        match self.kubectl.get_json("pod", &pod, &ns)? {
            None => Ok(None),
            Some(v) => Ok(Some((
                None,
                v["status"]["podIP"].as_str().map(str::to_string),
            ))),
        }
    }
}

/// `instance_id` 形如 `teamx-m-xxxxxxxx`；namespace 由调用方补全时用 `ns/pod`。
fn split_instance(instance_id: &str) -> (String, String) {
    match instance_id.split_once('/') {
        Some((ns, pod)) => (ns.to_string(), pod.to_string()),
        None => ("default".to_string(), instance_id.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_short_and_collision_free() {
        let a = "474a5a6b-2440-4c1c-b24c-4d2044589b65";
        let b = "71667b0d-7781-425f-aa83-dc80be84d429";
        assert_eq!(short_id(a), "474a5a6b24404c1c");
        assert!(short_id(a).len() >= 16, "M5: 命名需 ≥16 字符防碰撞");
        assert_ne!(pod_name(a), pod_name(b));
        assert_eq!(namespace_for("5eebe88f-1f9c-490f-9707-c8f01347e92c"), "tenant-5eebe88f1f9c490f");
        assert_eq!(pvc_name(a), "teamx-ws-474a5a6b24404c1c");
        assert_eq!(secret_name(a), "teamx-letter-474a5a6b24404c1c");
        // 空 id 不产生空名字（防撞成同一个空名）
        assert!(!pod_name("").is_empty());
    }

    #[test]
    fn provision_manifests_have_pvc_secret_but_no_pod() {
        let y = provision_manifests("t1", "m1", "teamx-inv:v1:AAAA", 30, None);
        assert!(y.contains("kind: PersistentVolumeClaim"));
        assert!(y.contains("kind: Secret"));
        assert!(y.contains("kind: Namespace"));
        // 裁决 V：provision 阶段**不建 Pod**
        assert!(!y.contains("kind: Pod"), "provision must not create the Pod");
        // PSA restricted 标注
        assert!(y.contains("pod-security.kubernetes.io/enforce: restricted"));
        // 邀请函内联（缩进后的 stringData）
        assert!(y.contains("invitation.letter: |"));
        assert!(y.contains("    teamx-inv:v1:AAAA"));
        // 存储规格
        assert!(y.contains("storage: 30Gi"));
    }

    #[test]
    fn member_pod_has_all_containers_and_hardening() {
        let y = member_pod_manifest("t1", "m1", "https://s:5783", "https://s/git/t/r", "net:m1");
        for c in ["name: setup", "name: main", "name: ttyd", "name: fs", "name: expose"] {
            assert!(y.contains(c), "missing {c}");
        }
        // 安全默认档（D17）
        assert!(y.contains("seccompProfile"));
        assert!(y.contains("readOnlyRootFilesystem: true"));
        assert!(y.contains("runAsNonRoot: true"));
        assert!(y.contains("drop: [\"ALL\"]"));
        // 固定挂载点 + 上报唯一事实源（D3）
        assert!(y.contains("mountPath: /workspace"));
        // 边车存活探针（D5 硬约束 2）
        assert!(y.contains("livenessProbe"));
        // 幂等 setup
        assert!(y.contains("if [ -d /workspace/.git ]"));
        assert!(y.contains("teamx team import"));
    }

    #[test]
    fn quota_manifests_land_in_k8s_layer() {
        let y = quota_manifests("t1");
        assert!(y.contains("kind: ResourceQuota"));
        assert!(y.contains("kind: LimitRange"));
        assert!(y.contains("persistentvolumeclaims"), "PVC 上限要有");
        assert!(y.contains("namespace: tenant-"), "必须落在租户 namespace");
    }

    #[test]
    fn setup_initcontainer_is_idempotent_locked() {
        let y = member_pod_manifest("t1", "m1", "https://s:5783", "https://s/git/t/r", "net:m1");
        assert!(y.contains("setup.lock"), "需要幂等锁");
        assert!(y.contains("mkdir \"$LOCK\""), "锁必须是原子 mkdir");
        assert!(y.contains("trap"), "退出要释放锁");
    }

    #[test]
    fn empty_allowlist_emits_explicit_empty_egress() {
        // 空白名单必须是 `egress: []`（不是空值，否则 API 拒绝）
        let y = network_policy_manifest("t1", "m1", &[]);
        assert!(y.contains("egress: []"), "empty allowlist must be an explicit empty list: {y}");
        assert!(!y.contains("egress:\n"), "must not emit a bare `egress:` key");
    }

    #[test]
    fn tenant_manifests_carry_ca_pvc_and_single_replica_server() {
        let y = tenant_manifests("t1", "CA-CRT", "CA-KEY", "SRV-CRT", "SRV-KEY");
        // 三元组的 namespace 侧
        assert!(y.contains("name: tenant-t1"), "namespace");
        assert!(y.contains("pod-security.kubernetes.io/enforce: restricted"));
        // 独立 CA（D2）：cert+key+server 证书都在同一个 Secret 里
        assert!(y.contains("kind: Secret"));
        assert!(y.contains("ca.crt: |"));
        assert!(y.contains("ca.key: |"));
        assert!(y.contains("server.crt: |"));
        assert!(y.contains("CA-KEY"), "CA key 必须注入（invite 时签发成员证书要用）");
        // server：单副本 + Recreate（SQLite 单写者）+ PVC + 只读 CA 挂载
        assert!(y.contains("kind: Deployment"));
        assert!(y.contains("replicas: 1"));
        assert!(y.contains("type: Recreate"));
        assert!(y.contains("kind: PersistentVolumeClaim"));
        assert!(y.contains("mountPath: /data/ca, readOnly: true"));
        assert!(y.contains("TEAMX_TENANT_TEAM_ID"));
        // 建议②（weiye task-92）：租户 server 用 server 档 PriorityClass + PDB maxUnavailable=0
        assert!(y.contains("kind: PriorityClass"), "server tier priority class");
        assert!(y.contains("priorityClassName: teamx-tenant-server"));
        assert!(y.contains("kind: PodDisruptionBudget"));
        assert!(y.contains("maxUnavailable: 0"), "单副本控制面不得被自愿中断驱逐");
        // 真集群修正：Service（服务发现）+ exec 探针（mTLS 下 HTTPS 探针必失败）
        assert!(y.contains("kind: Service"), "tenant server Service");
        assert!(y.contains("teamx.io/tenant-server: \"t1\""), "Service selector");
        assert!(!y.contains("scheme: HTTPS"), "mTLS 下不得用 HTTPS 探针");
        assert!(y.contains("/data/.teamx-ready"), "exec 探针读就绪文件");
        // 加固
        assert!(y.contains("readOnlyRootFilesystem: true"));
        assert!(y.contains("seccompProfile"));
    }

    #[test]
    fn network_policy_blocks_imds() {
        let y = network_policy_manifest("t1", "m1", &["10.0.0.0/8".to_string()]);
        assert!(y.contains("kind: NetworkPolicy"));
        // NetworkPolicy 是 allow 语义：封 IMDS = 不放进白名单（默认拒绝覆盖）。
        // 断言它不作为**允许目标**出现（注释里提到不算）。
        assert!(!y.contains("cidr: 169.254.169.254"), "IMDS must NOT be allow-listed");
        assert!(y.contains("policyTypes: [\"Egress\"]"), "egress must default-deny");
        assert!(y.contains("10.0.0.0/8"));
    }
}
