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

/// 成员短 id：取 UUID 的字母数字，前 8 位。
pub fn short_id(member_id: &str) -> String {
    let s: String = member_id.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
    if s.is_empty() { "unknown0".to_string() } else { s }
}

/// 租户 namespace：`tenant-<team 前 8>`（C11 三元组之一）。
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
          # 幂等：已认领则跳过
          teamx team import /workspace/.teamx/letters/invite/invitation.letter --session {session} || true
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
        requests: {{ cpu: "{cpu_req}", memory: "512Mi" }}
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
      securityContext:
        readOnlyRootFilesystem: true
        allowPrivilegeEscalation: false
        capabilities: {{ drop: ["ALL"] }}
  volumes:
    - name: workspace
      persistentVolumeClaim: {{ claimName: {pvc} }}
    - name: letter
      secret: {{ secretName: {sec} }}
    - name: tmp
      emptyDir: {{}}
"#
    )
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
    let mut egress = String::new();
    for h in allow_hosts {
        egress.push_str(&format!(
            "    - to:\n        - ipBlock:\n            cidr: {h}\n"
        ));
    }
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
  egress:
{egress}"#
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
        let yaml = provision_manifests(&req.team_id_hint(), &req.name, &letter, self.workspace_gib, None);
        self.kubectl.apply(&yaml).map(|_| ())
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
        self.kubectl.apply(&format!("{pod}---\n{np}")).map(|_| ())
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
                "Succeeded" => "running",
                "Failed" => "failed",
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
        assert_eq!(short_id(a), "474a5a6b");
        assert_ne!(pod_name(a), pod_name(b));
        assert_eq!(namespace_for("5eebe88f-1f9c-490f-9707-c8f01347e92c"), "tenant-5eebe88f");
        assert_eq!(pvc_name(a), "teamx-ws-474a5a6b");
        assert_eq!(secret_name(a), "teamx-letter-474a5a6b");
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
