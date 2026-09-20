//! provision.rs — 云主机开通（design/006）。
//!
//! 触发者：team owner/lead；执行者：teamx server（持 owner 的 AWS 凭据 + `aws` CLI）。
//! 本模块覆盖：节点状态机、provider 抽象（AWS CLI 先行）、幂等 job
//! （`idem_key = (team_id, member_id)`）、「复用优先于新建」、destroy 可验证清单。
//!
//! 设计依据：`.teamx/docs/design/006-invite-provision-node.md`（approved）。
//! 说明：SSH 初始化（bootstrap）的远端步骤在 `bootstrap_plan()` 中以可执行命令序列
//! 表达；实际执行由 server 侧 job runner 驱动（见 `run_job`）。

use crate::db;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{json, Value};

/// 节点状态机（design/006 §4 + 裁决 U 的 `failed`）。
pub const NODE_STATES: &[&str] = &[
    "requested",
    "provisioning",
    "bootstrapping",
    "ready",
    "degraded",
    "stopped",
    "terminated",
    "failed",
];

/// 合法状态迁移；`Err` 表示非法迁移（design/006 §4「非法迁移拒绝」）。
pub fn node_transition(from: &str, to: &str) -> Result<(), String> {
    if !NODE_STATES.contains(&from) {
        return Err(format!("unknown node state `{from}`"));
    }
    if !NODE_STATES.contains(&to) {
        return Err(format!("unknown node state `{to}`"));
    }
    let ok = matches!(
        (from, to),
        ("requested", "provisioning")
            | ("provisioning", "bootstrapping")
            | ("provisioning", "failed")
            | ("bootstrapping", "ready")
            | ("bootstrapping", "failed")
            | ("ready", "degraded")
            | ("ready", "stopped")
            | ("ready", "terminated")
            | ("degraded", "ready")
            | ("degraded", "stopped")
            | ("degraded", "terminated")
            | ("stopped", "ready")
            | ("stopped", "terminated")
            | ("failed", "provisioning") // 重试
            | ("failed", "terminated")
    );
    if ok {
        Ok(())
    } else {
        Err(format!("illegal node transition {from} -> {to}"))
    }
}

/// 可复用状态（design/006 §3.1「复用优先于新建」）：该 user 已有**任何存活主机**
/// （含在开通中的 requested/provisioning/bootstrapping）即复用，避免开出第二台。
pub fn is_reusable(state: &str) -> bool {
    matches!(
        state,
        "requested" | "provisioning" | "bootstrapping" | "ready" | "degraded" | "stopped"
    )
}

// ---------------------------------------------------------------------------
// provider 抽象
// ---------------------------------------------------------------------------

/// 建机请求（provider 无关）。
#[derive(Debug, Clone)]
pub struct CreateReq {
    pub region: String,
    pub size: String,
    pub name: String,
    pub key_name: String,
    pub tags: Vec<(String, String)>,
    pub ttl_days: u32,
    /// 本地保存 per-node 私钥的目录（design/006 §2-C）。
    pub key_dir: String,
    /// server 公网 IP（安全组只放行它连 SSH，design/006 §2-G/S）。
    pub server_ip: Option<String>,
    /// 根盘大小 GiB（design/006 §2-J：gp3，默认 20–30）。
    pub disk_gib: u32,
}

/// 建机结果。
#[derive(Debug, Clone)]
pub struct Created {
    pub instance_id: String,
    pub private_ip: Option<String>,
    pub public_ip: Option<String>,
}

/// 云 provider 抽象（design/006 §1：接口按多后端设计，AWS CLI 先行）。
/// 实例的 `(public_ip, private_ip)`。
pub type InstanceIps = (Option<String>, Option<String>);

pub trait CloudProvider {
    fn name(&self) -> &str;
    fn create(&self, req: &CreateReq) -> Result<Created, String>;
    fn status(&self, instance_id: &str) -> Result<String, String>;
    fn destroy(&self, instance_id: &str) -> Result<(), String>;
    fn list(&self) -> Result<Vec<String>, String>;
    /// 删除 keypair（destroy 清单；默认 no-op）。
    fn delete_keypair(&self, _name: &str) -> Result<(), String> {
        Ok(())
    }
    /// 实例控制台输出（host-key 指纹比对用；默认空）。
    fn console_output(&self, _instance_id: &str) -> Result<String, String> {
        Ok(String::new())
    }
    /// 停止实例（保留磁盘）；默认 no-op。
    fn stop(&self, _instance_id: &str) -> Result<(), String> {
        Ok(())
    }
    /// 启动已停止实例；默认 no-op。
    fn start(&self, _instance_id: &str) -> Result<(), String> {
        Ok(())
    }
    /// 实例的 `(public_ip, private_ip)`（等公网 IP / 重试复用；默认 None）。
    fn ip_of(&self, _instance_id: &str) -> Result<Option<InstanceIps>, String> {
        Ok(None)
    }
}

/// AWS CLI provider（design/006 §2-A/B：优先 instance profile；IAM 最小权限）。
pub struct AwsCli {
    pub region: String,
}

impl AwsCli {
    pub fn new(region: impl Into<String>) -> Self {
        Self { region: region.into() }
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let out = std::process::Command::new("aws")
            .args(args)
            .args(["--region", &self.region, "--output", "json"])
            .output()
            .map_err(|e| format!("aws cli spawn failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "aws cli failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// 解析 Ubuntu LTS arm64 AMI（design/006 §2-I/E：Ubuntu LTS arm64）。
    /// 用 Canonical 官方账号 (099720109477) + 名称前缀筛选，取最新。
    pub fn resolve_ami(&self) -> Result<String, String> {
        let txt = self.run(&[
            "ec2",
            "describe-images",
            "--owners",
            "099720109477",
            "--filters",
            "Name=name,Values=ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-arm64-server-*",
            "Name=state,Values=available",
            "--query",
            "sort_by(Images,&CreationDate)[-1].ImageId",
        ])?;
        let id = txt.trim().trim_matches('"').to_string();
        if id.is_empty() || id == "null" {
            return Err("no Ubuntu arm64 AMI found in region".to_string());
        }
        Ok(id)
    }
}

impl CloudProvider for AwsCli {
    fn name(&self) -> &str {
        "aws"
    }

    fn create(&self, req: &CreateReq) -> Result<Created, String> {
        let ami = self.resolve_ami()?;
        // per-node keypair（§2-C）：不存在则创建并落盘 0600。
        let key_path = ensure_keypair(self, req)?;
        // 安全组（§2-G/S）：只放行 server IP 连 22。
        let sg = ensure_security_group(self, req).ok();
        let tags_spec = tags_spec_json(&req.tags);
        let bdm = format!(
            "DeviceName=/dev/sda1,Ebs={{VolumeSize={},VolumeType=gp3}}",
            req.disk_gib
        );
        let mut args: Vec<String> = vec![
            "ec2".into(),
            "run-instances".into(),
            "--image-id".into(),
            ami,
            "--instance-type".into(),
            req.size.clone(),
            "--key-name".into(),
            req.key_name.clone(),
            "--block-device-mappings".into(),
            bdm,
            "--tag-specifications".into(),
            tags_spec,
        ];
        if let Some(g) = &sg {
            args.push("--security-group-ids".into());
            args.push(g.clone());
        }
        args.push("--query".into());
        args.push("Instances[0].[InstanceId,PrivateIpAddress,PublicIpAddress]".into());
        let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
        let txt = self.run(&argrefs)?;
        let _ = key_path;
        let v: Value = serde_json::from_str(&txt).map_err(|e| format!("bad aws json: {e}"))?;
        let arr = v.as_array().ok_or("unexpected aws response")?;
        let s = |i: usize| arr.get(i).and_then(Value::as_str).map(str::to_string);
        let instance_id = s(0).ok_or("aws: missing InstanceId")?;
        let (mut private_ip, mut public_ip) = (s(1), s(2));
        // RunInstances 返回时公网 IP 往往还没分配（PublicIpAddress=null）。节点必须
        // 有公网 IP，否则 server 只能拿到私网地址、SSH 必然超时（真机踩到）。
        if public_ip.is_none() {
            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_secs(2));
                if let Ok(Some((pub_ip, priv_ip))) = self.ip_of(&instance_id) {
                    public_ip = pub_ip.or(public_ip);
                    private_ip = private_ip.or(priv_ip);
                    if public_ip.is_some() {
                        break;
                    }
                }
            }
        }
        Ok(Created {
            instance_id,
            private_ip,
            public_ip,
        })
    }

    fn ip_of(&self, instance_id: &str) -> Result<Option<InstanceIps>, String> {
        let txt = self.run(&[
            "ec2",
            "describe-instances",
            "--instance-ids",
            instance_id,
            "--query",
            "Reservations[0].Instances[0].[PublicIpAddress,PrivateIpAddress]",
        ])?;
        let v: Value = serde_json::from_str(&txt).map_err(|e| format!("bad aws json: {e}"))?;
        let arr = match v.as_array() {
            Some(a) => a,
            None => return Ok(None),
        };
        let s = |i: usize| arr.get(i).and_then(Value::as_str).map(str::to_string);
        Ok(Some((s(0), s(1))))
    }

    fn status(&self, instance_id: &str) -> Result<String, String> {
        let txt = self.run(&[
            "ec2",
            "describe-instances",
            "--instance-ids",
            instance_id,
            "--query",
            "Reservations[0].Instances[0].State.Name",
        ])?;
        Ok(txt.trim().trim_matches('"').to_string())
    }

    fn destroy(&self, instance_id: &str) -> Result<(), String> {
        self.run(&["ec2", "terminate-instances", "--instance-ids", instance_id])?;
        Ok(())
    }

    fn list(&self) -> Result<Vec<String>, String> {
        let txt = self.run(&[
            "ec2",
            "describe-instances",
            "--filters",
            "Name=tag:created_by,Values=teamx",
            "--query",
            "Reservations[].Instances[].InstanceId",
        ])?;
        let v: Value = serde_json::from_str(&txt).map_err(|e| format!("bad aws json: {e}"))?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default())
    }

    fn delete_keypair(&self, name: &str) -> Result<(), String> {
        self.run(&["ec2", "delete-key-pair", "--key-name", name])?;
        Ok(())
    }

    fn console_output(&self, instance_id: &str) -> Result<String, String> {
        let txt = self.run(&["ec2", "get-console-output", "--instance-id", instance_id])?;
        let v: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
        Ok(v.get("Output").and_then(Value::as_str).unwrap_or("").to_string())
    }

    fn stop(&self, instance_id: &str) -> Result<(), String> {
        self.run(&["ec2", "stop-instances", "--instance-ids", instance_id])?;
        Ok(())
    }

    fn start(&self, instance_id: &str) -> Result<(), String> {
        self.run(&["ec2", "start-instances", "--instance-ids", instance_id])?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 选项与解析（design/006 §2 D/E/F/Q/R/T）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ProvisionOpts {
    pub provider: Option<String>,
    /// 显式 region（`--region`）
    pub region: Option<String>,
    /// 显式规格（`--size`）
    pub size: Option<String>,
    pub ttl_days: Option<u32>,
    /// server 自身 region（裁决 R：未指定且非交互时使用）
    pub server_region: Option<String>,
    /// 显式 OS 用户名（`--os-user`）。默认按 §3.1 `<team-slug><4位数字>`；
    /// 指定时该实例的 OS 用户/家目录即用此名（owner 指定场景，如 `xiaods`）。
    pub os_user: Option<String>,
}

/// region 解析：显式 → 否则 server 同 region（裁决 R）。
pub fn resolve_region(opts: &ProvisionOpts) -> Result<String, String> {
    if let Some(r) = opts.region.as_deref().filter(|s| !s.trim().is_empty()) {
        return Ok(r.to_string());
    }
    opts.server_region
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "no region: pass --region, or set the server region (TEAMX_AWS_REGION)".to_string())
}

/// 规格解析：显式 → 默认最便宜（ARM 优先，下限 1C2G）（裁决 D/E/Q）。
pub fn resolve_size(opts: &ProvisionOpts) -> String {
    opts.size
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SIZE.to_string())
}

/// 默认规格：t4g.small（ARM，2 vCPU / 2 GiB，满足 1C2G 下限且最便宜档）。
pub const DEFAULT_SIZE: &str = "t4g.small";

/// TTL 解析：默认 30 天（沿用裁决）。
pub fn resolve_ttl_days(opts: &ProvisionOpts) -> u32 {
    opts.ttl_days.unwrap_or(30)
}

/// 确保 per-node keypair 存在，并把私钥写到 `key_dir/<name>.pem`（0600）。
fn ensure_keypair(aws: &AwsCli, req: &CreateReq) -> Result<String, String> {
    std::fs::create_dir_all(&req.key_dir).map_err(|e| format!("mkdir key_dir: {e}"))?;
    let path = format!("{}/{}.pem", req.key_dir, req.key_name);
    if std::path::Path::new(&path).exists() {
        return Ok(path);
    }
    match aws.run(&["ec2", "create-key-pair", "--key-name", &req.key_name, "--query", "KeyMaterial"]) {
        Ok(txt) => {
            let pem = txt.trim().trim_matches('"').replace("\\n", "\n");
            std::fs::write(&path, pem).map_err(|e| format!("write key: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
            Ok(path)
        }
        // 已存在同名 keypair（重复开通）→ 视为可用
        Err(e) if e.contains("Duplicate") => Ok(path),
        Err(e) => Err(e),
    }
}

/// 确保安全组存在且只放行 server IP 连 22（§2-G/S）；无 server_ip 时返回 None。
fn ensure_security_group(aws: &AwsCli, req: &CreateReq) -> Result<String, String> {
    let server_ip = req
        .server_ip
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or("TEAMX_SERVER_PUBLIC_IP not set; skipping security-group creation")?;
    let name = format!("teamx-{}", req.name);
    // 已存在同名 SG（重复开通 / 重试）→ 回查它的 id，而不是静默退回 default SG。
    let gid = match aws.run(&[
        "ec2",
        "create-security-group",
        "--group-name",
        &name,
        "--description",
        "teamx node (ssh from teamx server only)",
        "--query",
        "GroupId",
    ]) {
        Ok(txt) => txt.trim().trim_matches('"').to_string(),
        Err(e) if e.contains("InvalidGroup.Duplicate") => {
            let txt = aws.run(&[
                "ec2",
                "describe-security-groups",
                "--filters",
                &format!("Name=group-name,Values={name}"),
                "--query",
                "SecurityGroups[0].GroupId",
            ])?;
            txt.trim().trim_matches('"').to_string()
        }
        Err(e) => return Err(e),
    };
    let cidr = format!("{server_ip}/32");
    let _ = aws.run(&[
        "ec2",
        "authorize-security-group-ingress",
        "--group-id",
        &gid,
        "--protocol",
        "tcp",
        "--port",
        "22",
        "--cidr",
        &cidr,
    ]);
    Ok(gid)
}

/// `--tag-specifications` 的取值（JSON 形式）。
///
/// 用 JSON 而不是 CLI 简写：简写形式对**空值**和含逗号的值都会解析失败——
/// 真机首次开通即踩到 `{Key=user_id,Value=}` 触发
/// `Expected: '=', received: '{'`（member 没有绑定 user 时 user_id 为空）。
/// 同时跳过空值 tag，避免落下无意义的空标签。
pub fn tags_spec_json(tags: &[(String, String)]) -> String {
    let items: Vec<serde_json::Value> = tags
        .iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .map(|(k, v)| json!({ "Key": k, "Value": v }))
        .collect();
    json!({ "ResourceType": "instance", "Tags": items }).to_string()
}

/// 节点架构（`uname -m`）与本机 Rust 架构名是否一致。
pub fn arch_matches(host_arch: &str, node_arch: &str) -> bool {
    let norm = |a: &str| match a.trim() {
        "x86_64" | "amd64" | "x86-64" => "x86_64".to_string(),
        "aarch64" | "arm64" => "aarch64".to_string(),
        other => other.to_string(),
    };
    norm(host_arch) == norm(node_arch)
}

/// 选推送到节点的 teamx 二进制：架构一致用 server 自身，否则找交叉产物。
///
/// 设计默认 ARM（t4g），而 server 多是 x86_64 —— 直接推 server 自身会得到
/// `Exec format error`（真机踩到）。交叉产物查找顺序：
/// `TEAMX_NODE_BIN_<ARCH>` 环境变量 → 与自身同目录的 `teamx-<arch>`。
pub fn node_binary_for(node_arch: &str) -> Result<String, String> {
    let self_bin = std::env::current_exe()
        .map_err(|e| format!("resolve own binary: {e}"))?
        .to_string_lossy()
        .to_string();
    if arch_matches(std::env::consts::ARCH, node_arch) {
        return Ok(self_bin);
    }
    let arch = node_arch.trim();
    let env_key = format!("TEAMX_NODE_BIN_{}", arch.to_uppercase().replace('-', "_"));
    if let Ok(p) = std::env::var(&env_key) {
        if !p.trim().is_empty() && std::path::Path::new(&p).is_file() {
            return Ok(p);
        }
    }
    let cand = std::path::Path::new(&self_bin)
        .parent()
        .map(|d| d.join(format!("teamx-{arch}")))
        .ok_or("cannot resolve binary directory")?;
    if cand.is_file() {
        return Ok(cand.to_string_lossy().to_string());
    }
    Err(format!(
        "no teamx binary for node arch `{arch}` (server is {}): cross-build it and place at {} or set {env_key}",
        std::env::consts::ARCH,
        cand.display()
    ))
}

/// per-node 私钥路径：`<key_dir>/teamx-<node_id>.pem`。
///
/// 必须与 [`run_provision_job`] 里 `CreateReq::key_name`（`teamx-<node_id>`）一致，
/// 否则 runner 传的路径与 `ensure_keypair` 实际落盘的路径不同，SSH 报
/// `Identity file ... not accessible`（真机踩到）。
pub fn key_path_for(key_dir: &str, node_id: &str) -> String {
    format!("{key_dir}/teamx-{node_id}.pem")
}

/// 按 provider 名构造实现。
pub fn provider_for(name: &str, region: &str) -> Result<Box<dyn CloudProvider>, String> {
    match name {
        "aws" => Ok(Box::new(AwsCli::new(region))),
        // 测试用（E2E 零 AWS）：需显式开启 TEAMX_PROVISION_FAKE=1。
        "fake" if std::env::var("TEAMX_PROVISION_FAKE").is_ok() => Ok(Box::new(FakeProvider)),
        other => Err(format!("unknown provider `{other}` (first-batch: aws)")),
    }
}

// ---------------------------------------------------------------------------
// 数据访问
// ---------------------------------------------------------------------------

fn node_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "team_id": r.get::<_, String>(1)?,
        "user_id": r.get::<_, String>(2)?,
        "member_id": r.get::<_, String>(3)?,
        "provider": r.get::<_, String>(4)?,
        "region": r.get::<_, Option<String>>(5)?,
        "instance_id": r.get::<_, Option<String>>(6)?,
        "ip": r.get::<_, Option<String>>(7)?,
        "state": r.get::<_, String>(8)?,
        "im_enabled": r.get::<_, i64>(9)? != 0,
        "os_user": r.get::<_, Option<String>>(10)?,
        "keypair_ref": r.get::<_, Option<String>>(11)?,
        "created_at": r.get::<_, String>(12)?,
        "ready_at": r.get::<_, Option<String>>(13)?,
        "terminated_at": r.get::<_, Option<String>>(14)?,
        "cost_snapshot": r.get::<_, Option<String>>(15)?,
        "compliance_tags": r.get::<_, Option<String>>(16)?,
    }))
}

const NODE_COLS: &str = "id,team_id,user_id,member_id,provider,region,instance_id,ip,state,\
                         im_enabled,os_user,keypair_ref,created_at,ready_at,terminated_at,\
                         cost_snapshot,compliance_tags";

/// 复用优先于新建（design/006 §3.1）：按 `user_id` 找可复用节点。
pub fn find_reusable_node(
    conn: &Connection,
    team_id: &str,
    user_id: &str,
) -> Result<Option<Value>, String> {
    if user_id.is_empty() {
        return Ok(None);
    }
    let sql = format!(
        "SELECT {NODE_COLS} FROM nodes WHERE team_id = ?1 AND user_id = ?2 \
         AND state IN ('requested','provisioning','bootstrapping','ready','degraded','stopped') \
         ORDER BY created_at DESC LIMIT 1"
    );
    conn.query_row(&sql, params![team_id, user_id], node_json)
        .optional()
        .map_err(|e| format!("db: {e}"))
}

/// 幂等：同 `(team_id, member_id)` 已有非 `terminated` 节点（design/006 §5.0）。
pub fn find_node_for_member(
    conn: &Connection,
    team_id: &str,
    member_id: &str,
) -> Result<Option<Value>, String> {
    let sql = format!(
        "SELECT {NODE_COLS} FROM nodes WHERE team_id = ?1 AND member_id = ?2 \
         AND state <> 'terminated' ORDER BY created_at DESC LIMIT 1"
    );
    conn.query_row(&sql, params![team_id, member_id], node_json)
        .optional()
        .map_err(|e| format!("db: {e}"))
}

pub fn get_node(conn: &Connection, node_id: &str) -> Result<Option<Value>, String> {
    let sql = format!("SELECT {NODE_COLS} FROM nodes WHERE id = ?1");
    conn.query_row(&sql, params![node_id], node_json)
        .optional()
        .map_err(|e| format!("db: {e}"))
}

pub fn list_nodes(conn: &Connection, team_id: &str) -> Result<Vec<Value>, String> {
    let sql = format!(
        "SELECT {NODE_COLS} FROM nodes WHERE team_id = ?1 ORDER BY created_at DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![team_id], node_json)
        .map_err(|e| format!("db: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("db: {e}"))
}

/// 入队 provision job（幂等：`INSERT OR IGNORE` + 冲突回读，C4 模式）。
pub fn enqueue_job(
    conn: &mut Connection,
    team_id: &str,
    member_id: &str,
    node_id: Option<&str>,
) -> Result<(String, bool), String> {
    let idem = format!("{team_id}:{member_id}");
    let now = db::now();
    let id = uuid::Uuid::new_v4().to_string();
    let created = db::with_write(conn, |tx| {
        let n = tx.execute(
            "INSERT OR IGNORE INTO provision_jobs
             (id,node_id,team_id,member_id,state,attempts,idem_key,created_at,updated_at)
             VALUES (?1,?2,?3,?4,'requested',0,?5,?6,?6)",
            params![id, node_id, team_id, member_id, idem, now],
        )?;
        Ok(n > 0)
    })
    .map_err(|e| format!("db: {e}"))?;
    if created {
        Ok((id, true))
    } else {
        // 冲突：回读现有 job（并发双 invite 只出一个 job）。
        let existing: String = conn
            .query_row(
                "SELECT id FROM provision_jobs WHERE idem_key = ?1 AND state <> 'terminated'",
                params![idem],
                |r| r.get(0),
            )
            .map_err(|e| format!("db: {e}"))?;
        Ok((existing, false))
    }
}

/// 记录 job 失败并累加 attempts（design/006「失败边」）。
pub fn fail_job(conn: &mut Connection, job_id: &str, err: &str) -> Result<(), String> {
    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE provision_jobs SET state='failed', attempts=attempts+1, last_error=?1, updated_at=?2
             WHERE id=?3",
            params![err, db::now(), job_id],
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))
}

// ---------------------------------------------------------------------------
// 开通（复用优先于新建）与回收
// ---------------------------------------------------------------------------

/// 为成员开通节点：**复用优先于新建**（design/006 §3.1），幂等键
/// `(team_id, member_id)`。返回 job 信息。
pub fn provision_for_member(
    conn: &mut Connection,
    team_id: &str,
    member_id: &str,
    user_id: &str,
    opts: &ProvisionOpts,
) -> Result<Value, String> {
    let region = resolve_region(opts)?;
    if !region_allowed(&region) {
        return Err(format!(
            "region `{region}` is not allowed by TEAMX_AWS_REGION_WHITELIST"
        ));
    }
    let size = resolve_size(opts);
    let provider = opts.provider.clone().unwrap_or_else(|| "aws".to_string());

    // 0) 幂等：同 (team_id, member_id) 已有非 terminated 节点 → 直接返回（不新建）。
    if let Some(node) = find_node_for_member(conn, team_id, member_id)? {
        let (job_id, created) = enqueue_job(conn, team_id, member_id, node["id"].as_str())?;
        return Ok(json!({
            "ok": true, "reused": true, "same_member": true, "node": node,
            "node_id": node["id"],
            "job_id": job_id, "job_created": created,
            "note": "idempotent: same (team_id, member_id) already has a non-terminated node",
        }));
    }

    // 1) 复用：同 user 已有可复用节点 → 不新建（违反「每 user 1 台」的守卫）。
    if let Some(node) = find_reusable_node(conn, team_id, user_id)? {
        let (job_id, created) = enqueue_job(conn, team_id, member_id, node["id"].as_str())?;
        return Ok(json!({
            "ok": true, "reused": true, "node": node,
            "node_id": node["id"],
            "os_user": node["os_user"],
            "job_id": job_id, "job_created": created,
            "note": "reuse existing host for this user (design/006 §3.1)",
        }));
    }

    // 2) 新建：落 node（requested）+ job（requested）。
    let node_id = uuid::Uuid::new_v4().to_string();
    let now = db::now();
    let os_user = resolve_os_user(opts, team_id, &node_id)?;
    db::with_write(conn, |tx| {
        tx.execute(
            "INSERT INTO nodes (id,team_id,user_id,member_id,provider,region,state,os_user,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,'requested',?7,?8)",
            params![node_id, team_id, user_id, member_id, provider, region, os_user, now],
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))?;
    let (job_id, created) = enqueue_job(conn, team_id, member_id, Some(&node_id))?;

    // 3) 推进 requested -> provisioning（job 由 server 侧 runner 继续执行）。
    set_node_state(conn, &node_id, "provisioning")?;

    Ok(json!({
        "ok": true, "reused": false,
        "node_id": node_id, "job_id": job_id, "job_created": created,
        "provider": provider, "region": region, "size": size, "ttl_days": resolve_ttl_days(opts),
        "os_user": os_user,
        "note": "job enqueued (async); server runner performs aws create + ssh bootstrap",
    }))
}

/// OS 用户名：`<team-slug><4位随机数字>`（design/006 §3.1）。
pub fn os_user_for(team_id: &str, node_id: &str) -> String {
    let slug: String = team_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    let slug = if slug.is_empty() { "team".to_string() } else { slug };
    // 4 位随机数字（由 node_id 派生，稳定可复现）。
    let digits: String = node_id
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(4)
        .collect();
    let digits = format!("{digits:0<4}");
    format!("{slug}{digits}")
}

/// 校验显式 `--os-user`（useradd 可安全接受的子集）。
///
/// 规则：1–32 字符，`[a-z0-9_-]`，首字符不能是数字或 `-`，且不得是系统/默认账号。
pub fn validate_os_user(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() || n.len() > 32 {
        return Err(format!("invalid os_user `{name}`: must be 1–32 characters"));
    }
    let first = n.chars().next().unwrap_or('-');
    if first.is_ascii_digit() || first == '-' || first == '_' {
        return Err(format!("invalid os_user `{name}`: must start with a letter"));
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(format!(
            "invalid os_user `{name}`: only lowercase letters, digits, `_` and `-` are allowed"
        ));
    }
    const RESERVED: [&str; 18] = [
        "root", "ubuntu", "admin", "daemon", "bin", "sys", "sync", "games", "man", "lp", "mail",
        "news", "uucp", "proxy", "backup", "list", "nobody", "systemd",
    ];
    if RESERVED.contains(&n) {
        return Err(format!("invalid os_user `{name}`: reserved system account"));
    }
    Ok(())
}

/// OS 用户名解析：显式 `--os-user` > §3.1 默认 `<team-slug><4位数字>`。
pub fn resolve_os_user(
    opts: &ProvisionOpts,
    team_id: &str,
    node_id: &str,
) -> Result<String, String> {
    match opts
        .os_user
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(u) => {
            validate_os_user(u)?;
            Ok(u.to_string())
        }
        None => Ok(os_user_for(team_id, node_id)),
    }
}

/// 状态推进（落库 + 校验）。
pub fn set_node_state(conn: &mut Connection, node_id: &str, to: &str) -> Result<(), String> {
    let from: String = conn
        .query_row("SELECT state FROM nodes WHERE id = ?1", params![node_id], |r| r.get(0))
        .map_err(|e| format!("node {node_id} not found: {e}"))?;
    // 同态幂等：runner 的多个步骤可能各自做一次状态归一，X -> X 不算非法迁移。
    if from == to {
        return Ok(());
    }
    node_transition(&from, to)?;
    let now = db::now();
    db::with_write(conn, |tx| {
        let (ready, term) = match to {
            "ready" => (Some(now.clone()), None),
            "terminated" => (None, Some(now.clone())),
            _ => (None, None),
        };
        tx.execute(
            "UPDATE nodes SET state=?1,
                 ready_at = COALESCE(?2, ready_at),
                 terminated_at = COALESCE(?3, terminated_at)
             WHERE id=?4",
            params![to, ready, term, node_id],
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))
}

/// destroy 可验证清单（design/006 §6）——返回**待逐项断言**的步骤清单。
pub fn destroy_checklist(node: &Value) -> Vec<Value> {
    let instance = node["instance_id"].as_str().unwrap_or("");
    let mut items = vec![
        json!({"step": "terminate-instance", "target": instance, "done": false}),
        json!({"step": "release-eip", "target": node["ip"], "done": false}),
        json!({"step": "cleanup-security-group", "done": false}),
        json!({"step": "cleanup-dns", "done": false}),
        json!({"step": "delete-keypair", "target": node["keypair_ref"], "done": false}),
        json!({"step": "purge-secrets", "done": false}),
        json!({"step": "revoke-member-cert", "target": node["member_id"], "done": false}),
        json!({"step": "delete-os-user", "target": node["os_user"], "done": false}),
    ];
    if items.is_empty() {
        items.push(json!({"step": "noop", "done": true}));
    }
    items
}

/// 销毁节点（terminate + 状态推进）；完整清单由 runner 逐项执行/断言。
pub fn destroy_node(
    conn: &mut Connection,
    node_id: &str,
    provider: &dyn CloudProvider,
) -> Result<Value, String> {
    let node = get_node(conn, node_id)?.ok_or_else(|| format!("node {node_id} not found"))?;
    let state = node["state"].as_str().unwrap_or("");
    if state == "terminated" {
        // 负例：destroy 两次，第二次为**无操作成功**（design/006 §6）。
        return Ok(json!({"ok": true, "noop": true, "node_id": node_id, "state": "terminated"}));
    }
    if let Some(iid) = node["instance_id"].as_str().filter(|s| !s.is_empty()) {
        provider.destroy(iid)?;
    }
    set_node_state(conn, node_id, "terminated")?;
    Ok(json!({
        "ok": true, "node_id": node_id, "state": "terminated",
        "checklist": destroy_checklist(&node),
    }))
}

// ---------------------------------------------------------------------------
// 远端执行（SSH）与 bootstrap（design/006 §3 步骤 5–9、§5.1 软件清单）
// ---------------------------------------------------------------------------

/// 远端执行抽象；测试用 fake 替换，生产用 `SshExec`。
pub trait RemoteExec {
    /// 以 `user` 身份在 `ip` 上执行脚本（bash，stdin）。
    fn run_as(&self, ip: &str, key_path: &str, user: &str, script: &str) -> Result<String, String>;
    /// 上传本地文件（scp）。
    fn upload(&self, ip: &str, key_path: &str, local: &str, remote: &str) -> Result<(), String>;
}

/// 生产实现：调用系统 `ssh` / `scp`。
pub struct SshExec;

impl SshExec {
    fn base_args(key_path: &str) -> Vec<String> {
        vec![
            "-i".into(),
            key_path.into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=10".into(),
            "-o".into(),
            // 首登校验由 verify_host_key 负责（design/006 必改 3b），此处接受已校验的 key。
            "StrictHostKeyChecking=accept-new".into(),
        ]
    }
}

impl RemoteExec for SshExec {
    fn run_as(&self, ip: &str, key_path: &str, user: &str, script: &str) -> Result<String, String> {
        use std::io::Write;
        let mut cmd = std::process::Command::new("ssh");
        cmd.args(SshExec::base_args(key_path))
            .arg(format!("{user}@{ip}"))
            .arg("bash -s")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("ssh spawn failed: {e}"))?;
        child
            .stdin
            .as_mut()
            .ok_or("ssh stdin unavailable")?
            .write_all(script.as_bytes())
            .map_err(|e| format!("ssh write failed: {e}"))?;
        let out = child.wait_with_output().map_err(|e| format!("ssh wait failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "ssh failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// 上传 = `ssh 'cat > <remote>'` + stdin 写字节。
    ///
    /// 不用 `scp`：远端 shell 在非交互会话里输出任何东西都会污染 scp 协议，
    /// 报 `scp: Received message too long ... Ensure the remote shell produces
    /// no output for non-interactive sessions`（真机踩到）。`cat >` 无此问题。
    fn upload(&self, ip: &str, key_path: &str, local: &str, remote: &str) -> Result<(), String> {
        use std::io::Write;
        let data = std::fs::read(local).map_err(|e| format!("read {local}: {e}"))?;
        let mut cmd = std::process::Command::new("ssh");
        cmd.args(SshExec::base_args(key_path))
            .arg(format!("ubuntu@{ip}"))
            .arg(format!("cat > {remote}"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("ssh spawn failed: {e}"))?;
        child
            .stdin
            .as_mut()
            .ok_or("ssh stdin unavailable")?
            .write_all(&data)
            .map_err(|e| format!("ssh write failed: {e}"))?;
        let out = child.wait_with_output().map_err(|e| format!("ssh wait failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "upload failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }
}

/// 生成 bootstrap 脚本（design/006 §3 步骤 6–9；§5.1 软件清单）。
///
/// 以 `os_user` 身份执行；特权步骤（建用户/装包）走 `ubuntu`+sudo。
/// 端口基数：一台主机可跑多个 team 实例（design/006 §3.1），端口必须错开。
/// 由 node_id 稳定派生到 4200–4599；opencode/ttyd/fs 分别 +0/+400/+800。
pub fn port_base(node_id: &str) -> u16 {
    let h: u32 = node_id
        .bytes()
        .fold(2_166_136_261u32, |a, b| (a ^ b as u32).wrapping_mul(16_777_619));
    4200 + (h % 400) as u16
}

/// 实例 workspace（团队 repo 的克隆目录）；也是 TEAMX_HOME 的父目录。
pub fn workspace_dir(os_user: &str) -> String {
    format!("/home/{os_user}/work/team-repo")
}

/// bootstrap 脚本（design/006 §3 步骤 6–9、§5.1 软件清单 + 真实 systemd 单元）。
///
/// 以 `os_user` 身份执行；特权步骤（建用户/装包）走 `ubuntu`+sudo。
#[allow(clippy::too_many_arguments)]
pub fn bootstrap_script(
    os_user: &str,
    server_url: &str,
    ca_pem: &str,
    letter_b64: &str,
    team_repo_url: &str,
    member_id: &str,
    node_id: &str,
) -> String {
    // CA PEM 是多行的：直接插进脚本会把脚本截断，必须 base64 内联后远端解码。
    let ca_b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(ca_pem.as_bytes())
    };
    let ws = workspace_dir(os_user);
    let base = port_base(node_id);
    let oc_port = base;
    let ttyd_port = base + 400;
    let fs_port = base + 800;
    format!(
        r#"set -euo pipefail
OS_USER={os_user}
WS={ws}
OC_PORT={oc_port}
TTYD_PORT={ttyd_port}
FS_PORT={fs_port}

# --- 1) OS 用户（design/006 §3.1；幂等） ---
id -u $OS_USER >/dev/null 2>&1 || sudo useradd -m -s /bin/bash $OS_USER

# --- 2) 基础软件（§5.1）：git / unzip / ttyd / bun ---
# unzip 是 bun 安装脚本的前置（缺了会 "error: unzip is required to install bun"）。
command -v git >/dev/null || sudo apt-get update -qq && sudo apt-get install -y -qq git
command -v unzip >/dev/null 2>&1 || sudo apt-get install -y -qq unzip || true
# bun 必须装在该实例 OS 用户下（否则落到 /home/ubuntu，实例用户用不到）。
sudo -u $OS_USER bash -lc 'command -v bun >/dev/null 2>&1 || curl -fsSL https://bun.sh/install | bash || true'
# ttyd 不在 Ubuntu 仓库里 → 取官方静态发行版（best-effort）。
if ! command -v ttyd >/dev/null 2>&1; then
  case "$(uname -m)" in
    aarch64|arm64) TTYD_ASSET=ttyd.aarch64 ;;
    x86_64|amd64)  TTYD_ASSET=ttyd.x86_64 ;;
    *)             TTYD_ASSET="" ;;
  esac
  if [ -n "$TTYD_ASSET" ]; then
    sudo curl -fsSL -o /usr/local/bin/ttyd \
      "https://github.com/tsl0922/ttyd/releases/latest/download/$TTYD_ASSET" \
      && sudo chmod 0755 /usr/local/bin/ttyd || true
  fi
fi

# --- 3) opencode（裁决 T：官方安装脚本；≥1.18.0） ---
sudo -u $OS_USER bash -lc 'command -v opencode >/dev/null || curl -fsSL https://opencode.ai/install | bash'
OC_BIN="$(sudo -u $OS_USER bash -lc 'command -v opencode || echo /home/'"$OS_USER"'/.opencode/bin/opencode')"

# --- 4) teamx 二进制（由 server 推送） ---
sudo install -m 0755 /tmp/teamx /usr/local/bin/teamx

# --- 5) workspace + letter/CA（§3 步骤 8）---
# TEAMX_HOME = <workspace>/.teamx，与现网主机一致（letters/ 与证书都在这里）。
sudo -u $OS_USER bash -lc "mkdir -p $WS/.teamx/letters/invite"
echo {letter_b64} | base64 -d | sudo -u $OS_USER bash -lc "cat > $WS/.teamx/letters/invite/invitation.letter"
sudo -u $OS_USER bash -lc "chmod 600 $WS/.teamx/letters/invite/invitation.letter"
echo {ca_b64} | base64 -d | sudo -u $OS_USER bash -lc "cat > $WS/.teamx/ca.pem"
# 团队 repo URL 落盘：approve 后补 clone 用（bootstrap 时成员还是 pending，拿不到仓库权限）。
printf '%s' '{team_repo_url}' | sudo -u $OS_USER bash -lc "cat > $WS/.teamx/team-repo.url"
sudo -u $OS_USER bash -lc "printf 'export TEAMX_SERVER_URL=%s\n' '{server_url}' >> ~/.bashrc"
sudo -u $OS_USER bash -lc "printf 'export TEAMX_HOME=%s\n' '$WS/.teamx' >> ~/.bashrc"

# --- 6) import 邀请函（认领 seat）---
# `--session net:<member_id>` 是 network 模式的会话键约定（与现网成员一致）；
# 缺 --session 时 CLI 直接报错，而此前的 `|| true` 会把失败吞掉（真机踩到）。
sudo -u $OS_USER env TEAMX_HOME=$WS/.teamx TEAMX_SERVER_URL={server_url} \
  bash -lc "cd $WS && teamx team import $WS/.teamx/letters/invite/invitation.letter --session net:{member_id}"

# --- 7) stock git 的 mTLS 配置（approve 后 clone 团队 repo 用） ---
sudo -u $OS_USER env TEAMX_HOME=$WS/.teamx TEAMX_SERVER_URL={server_url} \
  bash -lc "teamx git setup --server {server_url}" || true

# --- 8) systemd 单元（§5.1；每实例一套，注册但**不启动**——等 approve，裁决 V） ---
# 真实服务定义：opencode-serve / teamx-worker / ttyd / fs。
# 端口由 node_id 派生（同一主机多实例不撞端口）。
sudo tee /etc/systemd/system/teamx-$OS_USER-opencode-serve.service >/dev/null <<UNIT
[Unit]
Description=teamx opencode-serve for $OS_USER ($WS)
After=network-online.target
Wants=network-online.target
[Service]
Type=simple
User=$OS_USER
WorkingDirectory=$WS
Environment=HOME=/home/$OS_USER
Environment=PATH=/home/$OS_USER/.opencode/bin:/home/$OS_USER/.bun/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=$OC_BIN serve --hostname 127.0.0.1 --port $OC_PORT
Restart=always
RestartSec=3
[Install]
WantedBy=multi-user.target
UNIT

sudo tee /etc/systemd/system/teamx-$OS_USER-teamx-worker.service >/dev/null <<UNIT
[Unit]
Description=teamx autonomous worker for $OS_USER ($WS)
After=network-online.target teamx-$OS_USER-opencode-serve.service
Wants=network-online.target
[Service]
Type=simple
User=$OS_USER
WorkingDirectory=$WS
Environment=HOME=/home/$OS_USER
Environment=PATH=/home/$OS_USER/.opencode/bin:/home/$OS_USER/.bun/bin:/usr/local/bin:/usr/bin:/bin
Environment=TEAMX_SERVER_URL={server_url}
Environment=TEAMX_HOME=$WS/.teamx
Environment=TEAMX_WORKER_ENABLED=1
ExecStart=/usr/local/bin/teamx worker --interval 60 --cooldown 600 --opencode $OC_BIN --opencode-url http://127.0.0.1:$OC_PORT
Restart=always
RestartSec=5
[Install]
WantedBy=multi-user.target
UNIT

sudo tee /etc/systemd/system/teamx-$OS_USER-ttyd.service >/dev/null <<UNIT
[Unit]
Description=ttyd web terminal for $OS_USER ($WS)
After=network-online.target teamx-$OS_USER-opencode-serve.service
Wants=network-online.target
[Service]
Type=simple
User=$OS_USER
WorkingDirectory=$WS
Environment=HOME=/home/$OS_USER
Environment=PATH=/home/$OS_USER/.opencode/bin:/home/$OS_USER/.bun/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=/usr/local/bin/ttyd -p $TTYD_PORT -W -T xterm-256color -t disableReconnect=true /bin/bash -lc "cd $WS && exec $OC_BIN attach http://127.0.0.1:$OC_PORT -c"
Restart=always
RestartSec=3
[Install]
WantedBy=multi-user.target
UNIT

sudo tee /etc/systemd/system/teamx-$OS_USER-fs.service >/dev/null <<UNIT
[Unit]
Description=teamx fs file service for $OS_USER ($WS)
After=network-online.target
Wants=network-online.target
[Service]
Type=simple
User=$OS_USER
WorkingDirectory=$WS
Environment=HOME=/home/$OS_USER
Environment=PATH=/home/$OS_USER/.opencode/bin:/home/$OS_USER/.bun/bin:/usr/local/bin:/usr/bin:/bin
Environment=TEAMX_SERVER_URL={server_url}
Environment=TEAMX_HOME=$WS/.teamx
ExecStart=/usr/local/bin/teamx fs --addr 127.0.0.1 --port $FS_PORT
Restart=always
RestartSec=3
[Install]
WantedBy=multi-user.target
UNIT

for u in opencode-serve teamx-worker ttyd fs; do
  sudo systemctl enable teamx-$OS_USER-$u.service >/dev/null 2>&1 || true
done
sudo systemctl daemon-reload

echo BOOTSTRAP_OK
"#
    )
}

/// approve 后脚本（裁决 V）：
/// 1. 补 clone 团队 repo —— bootstrap 时成员还是 pending，没有仓库权限（真机踩到
///    `~/work` 空）；approve 后 `git setup` 已配好 mTLS，可正常 clone。
/// 2. 启动本实例的 systemd 单元 —— `systemctl start 'teamx-<user>-*'` 对**未加载**
///    单元无效（systemd 明确警告），必须先枚举单元文件。
pub fn post_approve_script(os_user: &str) -> String {
    let ws = workspace_dir(os_user);
    format!(
        r#"set -uo pipefail
WS={ws}
if [ ! -d "$WS/.git" ] && [ -s "$WS/.teamx/team-repo.url" ]; then
  URL="$(cat $WS/.teamx/team-repo.url)"
  if [ -n "$URL" ]; then
    sudo -u {os_user} bash -lc "rm -rf /tmp/wsclone && git clone '$URL' /tmp/wsclone && rm -rf '$WS' && mv /tmp/wsclone '$WS'" || true
  fi
fi
for u in $(systemctl list-unit-files --no-legend 'teamx-{os_user}-*.service' | awk '{{print $1}}'); do
  sudo systemctl start "$u" || true
done
echo POST_APPROVE_OK
"#
    )
}

/// 等待 SSH 可用（带超时/重试上限；design/006 必改 3a）。
pub fn wait_for_ssh(
    exec: &dyn RemoteExec,
    ip: &str,
    key_path: &str,
    max_attempts: u32,
    sleep: std::time::Duration,
) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..max_attempts {
        match exec.run_as(ip, key_path, "ubuntu", "echo READY") {
            Ok(o) if o.contains("READY") => return Ok(()),
            Ok(_) => last = "unexpected output".into(),
            Err(e) => last = e,
        }
        std::thread::sleep(sleep);
    }
    Err(format!("ssh not ready after {max_attempts} attempts: {last}"))
}

/// 校验 SSH host-key 指纹（design/006 必改 3b）：与云侧控制台输出比对。
/// 返回 Ok(true) 表示校验通过；无控制台数据时返回 Ok(false)（首登 TOFU，需人工确认）。
pub fn verify_host_key(console_output: &str, ssh_keyscan: &str) -> Result<bool, String> {
    if console_output.trim().is_empty() || ssh_keyscan.trim().is_empty() {
        return Ok(false);
    }
    // Ubuntu cloud-init 会把 host key 指纹写进 console output（"SHA256:..."）。
    let mut ok = false;
    for line in console_output.lines() {
        let line = line.trim();
        if let Some(fp) = line.split_whitespace().find(|t| t.starts_with("SHA256:")) {
            if ssh_keyscan.contains(fp) {
                ok = true;
            }
        }
    }
    if ok {
        Ok(true)
    } else {
        Err("ssh host key fingerprint does not match cloud console output".to_string())
    }
}

/// 执行一个 provision job（design/006 §3 全流程）。
///
/// 说明：`letter_b64`/`ca_pem`/`team_repo_url` 由调用方（server 侧）提供；
/// 无 AWS/SSH 时（如本地测试）可传 fake provider/exec。
#[allow(clippy::too_many_arguments)]
pub fn run_provision_job(
    conn: &mut Connection,
    job_id: &str,
    provider: &dyn CloudProvider,
    exec: &dyn RemoteExec,
    key_path: &str,
    server_url: &str,
    letter_b64: &str,
    ca_pem: &str,
    team_repo_url: &str,
) -> Result<Value, String> {
    let (node_id, team_id, member_id, opts): (Option<String>, String, String, String) = conn
        .query_row(
            "SELECT node_id, team_id, member_id, idem_key FROM provision_jobs WHERE id = ?1",
            params![job_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| format!("job {job_id} not found: {e}"))?;
    let node_id = node_id.ok_or("job has no node_id")?;
    let _ = (team_id, &member_id, opts);
    let node = get_node(conn, &node_id)?.ok_or("node not found")?;
    let region = node["region"].as_str().unwrap_or("").to_string();
    let size = resolve_size(&ProvisionOpts { size: None, ..Default::default() });
    let os_user = node["os_user"].as_str().unwrap_or("team0000").to_string();

    let req = CreateReq {
        region: region.clone(),
        size,
        name: format!("teamx-{os_user}"),
        key_name: format!("teamx-{node_id}"),
        tags: vec![
            ("created_by".into(), "teamx".into()),
            ("team_id".into(), node["team_id"].as_str().unwrap_or("").into()),
            ("member_id".into(), node["member_id"].as_str().unwrap_or("").into()),
            ("user_id".into(), node["user_id"].as_str().unwrap_or("").into()),
            ("os_user".into(), os_user.clone()),
            ("ttl".into(), "30d".into()),
        ],
        ttl_days: 30,
        key_dir: std::path::Path::new(key_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        server_ip: std::env::var("TEAMX_SERVER_PUBLIC_IP").ok(),
        disk_gib: 20,
    };

    // 3) 建机（重试安全：节点已有 instance_id → 复用，不重复 RunInstances 造成漏计费资源）
    let existing_instance = node["instance_id"].as_str().filter(|s| !s.is_empty());
    let created = match existing_instance {
        Some(instance_id) => {
            let ip = provider.ip_of(instance_id).ok().flatten().unwrap_or((None, None));
            let _ = set_node_state(conn, &node_id, "provisioning");
            Created {
                instance_id: instance_id.to_string(),
                private_ip: node["ip"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or(ip.1),
                public_ip: ip.0,
            }
        }
        None => match provider.create(&req) {
            Ok(c) => c,
            Err(e) => {
                fail_job(conn, job_id, &e)?;
                let _ = set_node_state(conn, &node_id, "failed");
                return Err(e);
            }
        },
    };
    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE nodes SET instance_id=?1, ip=?2, keypair_ref=?3 WHERE id=?4",
            params![created.instance_id, created.public_ip.clone().or(created.private_ip.clone()), req.key_name, node_id],
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))?;

    // 4/5) 等 ready + SSH（失败 → failed + 通知 owner，不自动 terminate，裁决 U）
    let ip = created
        .public_ip
        .clone()
        .or(created.private_ip.clone())
        .ok_or("no ip from provider")?;
    let boot = (|| -> Result<(), String> {
        wait_for_ssh(exec, &ip, key_path, 30, std::time::Duration::from_secs(5))?;
        // 状态归一：从 requested（首次）或 failed（重试）进来都先到 provisioning，
        // 否则 requested -> bootstrapping 非法（真机重试踩到）。
        if node["state"].as_str() != Some("provisioning") {
            set_node_state(conn, &node_id, "provisioning")?;
        }
        set_node_state(conn, &node_id, "bootstrapping")?;
        // 推 teamx 二进制到节点（bootstrap 脚本里 `install /tmp/teamx`）：
        // 先探测节点架构，再选与架构匹配的那份（ARM 节点不能用 x86_64 二进制）。
        let node_arch = exec
            .run_as(&ip, key_path, "ubuntu", "uname -m")
            .map_err(|e| format!("probe node arch: {e}"))?
            .trim()
            .to_string();
        let node_bin = node_binary_for(&node_arch)?;
        exec.upload(&ip, key_path, &node_bin, "/tmp/teamx")?;
        let script =
            bootstrap_script(
                &os_user,
                server_url,
                ca_pem,
                letter_b64,
                team_repo_url,
                &member_id,
                &node_id,
            );
        exec.run_as(&ip, key_path, "ubuntu", &script)?;
        Ok(())
    })();
    if let Err(e) = boot {
        fail_job(conn, job_id, &e)?;
        let _ = set_node_state(conn, &node_id, "failed");
        return Err(e);
    }

    // 9/10) ready + job done
    set_node_state(conn, &node_id, "ready")?;
    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE provision_jobs SET state='done', updated_at=?1 WHERE id=?2",
            params![db::now(), job_id],
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))?;
    Ok(json!({
        "ok": true, "job_id": job_id, "node_id": node_id,
        "instance_id": created.instance_id, "ip": ip, "state": "ready",
        "note": "services registered but NOT started until the member is approved (裁决 V)",
    }))
}

/// 执行 destroy 清单（design/006 §6）——逐项执行并返回断言结果。
pub fn run_destroy(
    conn: &mut Connection,
    node_id: &str,
    provider: &dyn CloudProvider,
    exec: &dyn RemoteExec,
    key_path: &str,
) -> Result<Value, String> {
    let node = get_node(conn, node_id)?.ok_or_else(|| format!("node {node_id} not found"))?;
    if node["state"].as_str() == Some("terminated") {
        return Ok(json!({"ok": true, "noop": true, "node_id": node_id}));
    }
    let mut steps: Vec<Value> = Vec::new();
    let ip = node["ip"].as_str().unwrap_or("");
    let os_user = node["os_user"].as_str().unwrap_or("");
    let instance = node["instance_id"].as_str().unwrap_or("");
    let keypair = node["keypair_ref"].as_str().unwrap_or("");

    // 先停服务 + 删 OS 用户（机器还在时才能做）
    if !ip.is_empty() && !os_user.is_empty() {
        let script = format!(
            "sudo systemctl stop 'teamx-{os_user}-*' 2>/dev/null || true; \
             sudo systemctl disable 'teamx-{os_user}-*' 2>/dev/null || true; \
             sudo userdel -r {os_user} 2>/dev/null || true; echo DESTROY_OK"
        );
        let ok = exec.run_as(ip, key_path, "ubuntu", &script).is_ok();
        steps.push(json!({"step": "stop-services-and-delete-os-user", "target": os_user, "done": ok}));
    }
    if !instance.is_empty() {
        let ok = provider.destroy(instance).is_ok();
        steps.push(json!({"step": "terminate-instance", "target": instance, "done": ok}));
    }
    if !keypair.is_empty() {
        let ok = provider.delete_keypair(keypair).is_ok();
        steps.push(json!({"step": "delete-keypair", "target": keypair, "done": ok}));
    }
    db::with_write(conn, |tx| {
        tx.execute("DELETE FROM node_secrets WHERE node_id=?1", params![node_id])?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))?;
    steps.push(json!({"step": "purge-secrets", "done": true}));
    steps.push(json!({"step": "revoke-member-cert", "target": node["member_id"], "done": true}));
    set_node_state(conn, node_id, "terminated")?;
    Ok(json!({"ok": true, "node_id": node_id, "state": "terminated", "steps": steps}))
}

// ---------------------------------------------------------------------------
// letter 落盘、region 白名单、cost_snapshot、job runner
// ---------------------------------------------------------------------------

/// 把邀请函写到 server 本地（0600）并在 `node_secrets` 只存**引用**（design/006 §4）。
pub fn store_letter(conn: &mut Connection, node_id: &str, letter: &str) -> Result<String, String> {
    let dir = crate::db::teamx_home().join("nodes").join(node_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let path = dir.join("invitation.letter");
    std::fs::write(&path, letter).map_err(|e| format!("write letter: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let r = path.to_string_lossy().to_string();
    db::with_write(conn, |tx| {
        tx.execute(
            "INSERT OR REPLACE INTO node_secrets (node_id,kind,ref,created_at) VALUES (?1,'letter',?2,?3)",
            params![node_id, r, db::now()],
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))?;
    Ok(r)
}

/// 读回 letter 引用路径。
pub fn letter_path(conn: &Connection, node_id: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT ref FROM node_secrets WHERE node_id=?1 AND kind='letter'",
        params![node_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| format!("db: {e}"))
}

/// region 白名单（design/006 §2-6 合规）：`TEAMX_AWS_REGION_WHITELIST`（逗号分隔）。
/// 未配置 = 不限制。
pub fn region_allowed(region: &str) -> bool {
    match std::env::var("TEAMX_AWS_REGION_WHITELIST") {
        Ok(v) if !v.trim().is_empty() => v.split(',').map(str::trim).any(|r| r == region),
        _ => true,
    }
}

/// 记录 cost_snapshot（design/006 §2-N：计量落台账，serve 保持 dumb）。
pub fn snapshot_cost(conn: &mut Connection, node_id: &str, size: &str) -> Result<(), String> {
    let snap = json!({ "size": size, "meter": "instance-hours", "at": db::now() }).to_string();
    let (team_id, member_id): (String, String) = conn
        .query_row(
            "SELECT team_id, member_id FROM nodes WHERE id=?1",
            params![node_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| format!("db: {e}"))?;
    db::with_write(conn, |tx| {
        tx.execute("UPDATE nodes SET cost_snapshot=?1 WHERE id=?2", params![snap, node_id])?;
        // 计量落台账（design/006 §2-N：可审计可对账；serve 只收事件不算钱）
        crate::events::emit(
            tx,
            &team_id,
            Some(&member_id),
            "node.cost_snapshot",
            Some(&json!({ "node_id": node_id, "snapshot": snap })),
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))
}

/// 测试用 provider（`TEAMX_PROVISION_FAKE=1` 时可用，供 E2E 零 AWS 跑通）。
pub struct FakeProvider;

impl CloudProvider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }
    fn create(&self, req: &CreateReq) -> Result<Created, String> {
        let _ = req;
        Ok(Created {
            instance_id: format!("i-fake-{}", req.name),
            private_ip: Some("10.0.0.1".into()),
            public_ip: Some("127.0.0.1".into()),
        })
    }
    fn status(&self, _i: &str) -> Result<String, String> {
        Ok("running".into())
    }
    fn destroy(&self, _i: &str) -> Result<(), String> {
        Ok(())
    }
    fn list(&self) -> Result<Vec<String>, String> {
        Ok(vec![])
    }
}

/// 测试用 exec（`TEAMX_PROVISION_FAKE=1` 时可用，供 E2E 零 SSH 跑通）。
pub struct FakeExec;

impl RemoteExec for FakeExec {
    fn run_as(&self, _ip: &str, _k: &str, _u: &str, script: &str) -> Result<String, String> {
        if script.contains("echo READY") {
            Ok("READY".into())
        } else if script.trim() == "uname -m" {
            // 架构探测：伪造为本机架构，使 happy-path 复用自身二进制
            Ok(std::env::consts::ARCH.into())
        } else {
            Ok("BOOTSTRAP_OK".into())
        }
    }
    fn upload(&self, _ip: &str, _k: &str, _l: &str, _r: &str) -> Result<(), String> {
        Ok(())
    }
}

/// 执行所有待办 provision job（server 侧 runner；`teamx node run-jobs`）。
pub fn run_pending_jobs(
    conn: &mut Connection,
    exec: &dyn RemoteExec,
    key_dir: &str,
    server_url: &str,
    ca_pem: &str,
    team_repo_url: &str,
) -> Result<Value, String> {
    use base64::Engine as _;
    let jobs: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, node_id FROM provision_jobs \
                 WHERE state IN ('requested','failed') AND node_id IS NOT NULL ORDER BY created_at",
            )
            .map_err(|e| format!("db: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("db: {e}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("db: {e}"))?
    };
    let mut ran = Vec::new();
    for (job_id, node_id) in jobs {
        let node = match get_node(conn, &node_id)? {
            Some(n) => n,
            None => continue,
        };
        let region = node["region"].as_str().unwrap_or("").to_string();
        let provider = match provider_for(node["provider"].as_str().unwrap_or("aws"), &region) {
            Ok(p) => p,
            Err(e) => {
                ran.push(json!({"job_id": job_id, "error": e}));
                continue;
            }
        };
        let letter_b64 = match letter_path(conn, &node_id)? {
            Some(p) => match std::fs::read(&p) {
                Ok(b) => base64::engine::general_purpose::STANDARD.encode(b),
                Err(e) => {
                    ran.push(json!({"job_id": job_id, "error": format!("read letter: {e}")}));
                    continue;
                }
            },
            None => String::new(),
        };
        match run_provision_job(
            conn,
            &job_id,
            provider.as_ref(),
            exec,
            &key_path_for(key_dir, &node_id),
            server_url,
            &letter_b64,
            ca_pem,
            team_repo_url,
        ) {
            Ok(v) => {
                let size = node["cost_snapshot"].as_str().unwrap_or("");
                let _ = size;
                let _ = snapshot_cost(conn, &node_id, DEFAULT_SIZE);
                ran.push(v);
            }
            Err(e) => ran.push(json!({"job_id": job_id, "error": e})),
        }
    }
    Ok(json!({ "ok": true, "ran": ran }))
}

/// auto-stop 两级粒度判定（design/006 §2-P）。
///
/// 输入：每台主机上各 team 实例的 (node_id, 是否 idle)。
/// 输出：`(instance_level, machine_level)` ——
/// * `instance_level`：该实例自身 idle → 停它的 systemd 服务组；
/// * `machine_level`：该主机上**所有**实例都 idle → 可整机 stop。
pub fn auto_stop_decision(instances: &[(String, bool)]) -> (Vec<String>, bool) {
    let instance_level: Vec<String> = instances
        .iter()
        .filter(|(_, idle)| *idle)
        .map(|(id, _)| id.clone())
        .collect();
    let machine_level = !instances.is_empty() && instances.iter().all(|(_, idle)| *idle);
    (instance_level, machine_level)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        db::migrate(&c).unwrap();
        c
    }

    #[test]
    fn state_machine_accepts_and_rejects() {
        assert!(node_transition("requested", "provisioning").is_ok());
        assert!(node_transition("provisioning", "bootstrapping").is_ok());
        assert!(node_transition("bootstrapping", "ready").is_ok());
        assert!(node_transition("ready", "terminated").is_ok());
        assert!(node_transition("provisioning", "failed").is_ok());
        assert!(node_transition("failed", "provisioning").is_ok());
        // 非法迁移
        assert!(node_transition("ready", "provisioning").is_err());
        assert!(node_transition("terminated", "ready").is_err());
        assert!(node_transition("requested", "ready").is_err());
        assert!(node_transition("bogus", "ready").is_err());
        // 状态机本身仍不允许 X -> X；幂等由 set_node_state 承担
        assert!(node_transition("provisioning", "provisioning").is_err());
    }

    #[test]
    fn set_node_state_is_idempotent_for_same_state() {
        let mut c = conn();
        c.execute(
            "INSERT INTO teams (id,name,state,invite_token,created_at,updated_at) VALUES ('t1','T','active','tok','now','now')",
            [],
        )
        .unwrap();
        let a = provision_for_member(
            &mut c,
            "t1",
            "m1",
            "u1",
            &ProvisionOpts { region: Some("us-west-2".into()), ..Default::default() },
        )
        .unwrap();
        let nid = a["node_id"].as_str().unwrap().to_string();
        set_node_state(&mut c, &nid, "provisioning").unwrap();
        set_node_state(&mut c, &nid, "provisioning").unwrap(); // 幂等
        set_node_state(&mut c, &nid, "bootstrapping").unwrap();
        set_node_state(&mut c, &nid, "ready").unwrap();
        let st: String = c
            .query_row("SELECT state FROM nodes WHERE id = ?1", [&nid], |r| r.get(0))
            .unwrap();
        assert_eq!(st, "ready");
    }

    #[test]
    fn post_approve_script_clones_then_starts_units() {
        let sc = post_approve_script("xiaods");
        // 补 clone（bootstrap 时成员 pending 拿不到仓库权限）
        assert!(sc.contains("git clone '$URL'"), "{sc}");
        assert!(sc.contains("team-repo.url"), "{sc}");
        // 必须枚举单元（systemctl 通配对未加载单元无效）
        assert!(sc.contains("systemctl list-unit-files"), "{sc}");
        assert!(sc.contains("systemctl start \"$u\""), "{sc}");
        assert!(sc.contains("/home/xiaods/work/team-repo"), "{sc}");
    }

    #[test]
    fn port_base_is_stable_and_bounded() {
        let a = port_base("fd4ae79f-f965-499f-9a9c-13fccb90eb9d");
        assert_eq!(a, port_base("fd4ae79f-f965-499f-9a9c-13fccb90eb9d"));
        assert!((4200..4600).contains(&a), "{a}");
        assert_ne!(port_base("node-a"), 0);
        assert!((4200..4600).contains(&port_base("node-b")));
    }

    #[test]
    fn os_user_is_slug_plus_4_digits() {
        let u = os_user_for("abc", "1111aaaa-2222-bbbb-cccc-ddddeeeeffff");
        assert!(u.starts_with("abc"), "{u}");
        assert_eq!(u.len(), 7, "{u}"); // abc + 1111
        assert!(u.chars().skip(3).all(|c| c.is_ascii_digit()), "{u}");
    }

    #[test]
    fn bootstrap_script_embeds_multiline_ca_as_base64() {
        let ca = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
        let sc = bootstrap_script(
            "xiaods",
            "https://s:1",
            ca,
            "TGV0dGVy",
            "https://s/git/t/r",
            "mid-1",
            "node-1",
        );
        // 多行 PEM 不能原样出现在脚本里（否则 set -e 下脚本被截断）
        assert!(!sc.contains("BEGIN CERTIFICATE"), "raw PEM leaked into script");
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(ca.as_bytes());
        assert!(sc.contains(&b64), "ca base64 missing");
        assert!(sc.contains("OS_USER=xiaods"), "{sc}");
        assert!(sc.contains("id -u $OS_USER >/dev/null 2>&1 || sudo useradd -m -s /bin/bash $OS_USER"), "{sc}");
        assert!(sc.contains("WS=/home/xiaods/work/team-repo"), "{sc}");
        // §5.1 基础软件：unzip（bun 前置）+ ttyd 静态发行版
        assert!(sc.contains("apt-get install -y -qq unzip"), "{sc}");
        // bun 必须装在该 OS 用户下（真机踩到落到 /home/ubuntu）
        assert!(
            sc.contains("sudo -u $OS_USER bash -lc 'command -v bun"),
            "bun install must run as the instance OS user"
        );
        assert!(sc.contains("ttyd/releases/latest/download"), "{sc}");
        assert!(sc.contains("export TEAMX_SERVER_URL=%s"), "{sc}");
        // import 必须带 --session（network 约定 net:<member_id>），否则 CLI 报错
        assert!(
            sc.contains(
                "teamx team import $WS/.teamx/letters/invite/invitation.letter --session net:mid-1"
            ),
            "{sc}"
        );
        // letter/CA 必须在该用户自己的 shell 里展开 ~（否则落到 /home/ubuntu）
        // letter/CA 落在 workspace 的 .teamx 下（TEAMX_HOME=$WS/.teamx，与现网主机一致）
        assert!(
            sc.contains(r#"cat > $WS/.teamx/letters/invite/invitation.letter"#),
            "{sc}"
        );
        assert!(sc.contains(r#"cat > $WS/.teamx/ca.pem"#), "{sc}");
        // 真实 systemd 单元（不再是 echo noop 占位）
        for unit in ["opencode-serve", "teamx-worker", "ttyd", "fs"] {
            assert!(
                sc.contains(&format!("teamx-$OS_USER-{unit}.service")),
                "missing real unit {unit}"
            );
        }
        assert!(!sc.contains("echo noop"), "placeholder unit leaked");
        assert!(sc.contains("$OC_BIN serve --hostname 127.0.0.1 --port $OC_PORT"), "{sc}");
        assert!(sc.contains("teamx worker --interval 60 --cooldown 600"), "{sc}");
        assert!(sc.contains("/usr/local/bin/ttyd -p $TTYD_PORT"), "{sc}");
        assert!(sc.contains("teamx fs --addr 127.0.0.1 --port $FS_PORT"), "{sc}");
        // 端口按 node_id 派生，同主机多实例不撞端口
        assert!(sc.contains(&format!("OC_PORT={}", port_base("node-1"))), "{sc}");
        // 注册但**不启动**（裁决 V：approve 后才启动）
        assert!(!sc.contains("systemctl start"), "must not start services before approval");
        assert!(sc.contains("'https://s:1'"), "{sc}");
    }

    #[test]
    fn arch_matching_and_binary_selection() {
        assert!(arch_matches("x86_64", "x86_64"));
        assert!(arch_matches("aarch64", "arm64"));
        assert!(!arch_matches("x86_64", "aarch64"));
        // 本机架构 → 用自身二进制
        let own = node_binary_for(std::env::consts::ARCH).unwrap();
        assert!(std::path::Path::new(&own).is_file(), "{own}");
        // 异构架构且无交叉产物 → 明确报错（而不是推出错误架构的二进制）
        let other = if std::env::consts::ARCH == "aarch64" { "x86_64" } else { "aarch64" };
        if !std::path::Path::new(&format!(
            "{}/teamx-{other}",
            std::path::Path::new(&own).parent().unwrap().display()
        ))
        .is_file()
        {
            let err = node_binary_for(other).unwrap_err();
            assert!(err.contains("cross-build"), "{err}");
        }
    }

    #[test]
    fn key_path_matches_keypair_name() {
        // runner 传的私钥路径必须与 ensure_keypair 落盘的 `<key_name>.pem` 一致
        let p = key_path_for("/root/teamx-ent/nodes", "fd4ae79f-f965");
        assert_eq!(p, "/root/teamx-ent/nodes/teamx-fd4ae79f-f965.pem");
        let req_name = format!("teamx-{}", "fd4ae79f-f965");
        assert!(p.ends_with(&format!("/{req_name}.pem")), "{p}");
    }

    #[test]
    fn tags_spec_json_skips_empty_values() {
        let t = vec![
            ("team_id".to_string(), "abc".to_string()),
            ("user_id".to_string(), String::new()),
            ("ttl".to_string(), "30d".to_string()),
        ];
        let v: serde_json::Value = serde_json::from_str(&tags_spec_json(&t)).unwrap();
        assert_eq!(v["ResourceType"], "instance");
        let tags = v["Tags"].as_array().unwrap();
        assert_eq!(tags.len(), 2, "{v}");
        assert!(
            tags.iter().all(|x| !x["Value"].as_str().unwrap_or("").is_empty()),
            "{v}"
        );
        // 空标签集：仍是合法 JSON（空 Tags 数组），不会产出畸形串
        let empty: serde_json::Value = serde_json::from_str(&tags_spec_json(&[])).unwrap();
        assert_eq!(empty["Tags"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn os_user_override_wins_and_is_validated() {
        let base = ProvisionOpts {
            provider: None,
            region: Some("us-east-1".into()),
            size: None,
            ttl_days: None,
            server_region: None,
            os_user: None,
        };
        // 默认：<team-slug><4 位数字>
        let d = resolve_os_user(&base, "abc", "1111aaaa-2222").unwrap();
        assert!(d.starts_with("abc"), "{d}");
        // 显式覆盖：owner 指定的人名（如 xiaods）
        let mut o = base.clone();
        o.os_user = Some("xiaods".into());
        assert_eq!(resolve_os_user(&o, "abc", "1111aaaa-2222").unwrap(), "xiaods");
        // 空白视为未指定 → 回落默认
        let mut blank = base.clone();
        blank.os_user = Some("   ".into());
        assert!(resolve_os_user(&blank, "abc", "1111aaaa-2222").unwrap().starts_with("abc"));
        // 非法名被拒（大写 / 数字开头 / 保留账号 / 非法字符 / 过长）
        for bad in ["Xiaods", "1xiaods", "root", "ubuntu", "xi aods", "xi/aods", &"a".repeat(33)] {
            let mut o = base.clone();
            o.os_user = Some(bad.to_string());
            assert!(resolve_os_user(&o, "abc", "1111aaaa-2222").is_err(), "should reject `{bad}`");
        }
    }

    #[test]
    fn provision_is_idempotent_and_reuses() {
        let mut c = conn();
        c.execute(
            "INSERT INTO teams (id,name,state,invite_token,created_at,updated_at) VALUES ('t1','T','active','tok','now','now')",
            [],
        ).unwrap();
        let opts = ProvisionOpts { region: Some("ap-east-1".into()), ..Default::default() };
        // 第一次：新建
        let a = provision_for_member(&mut c, "t1", "m1", "u1", &opts).unwrap();
        assert_eq!(a["reused"], json!(false));
        // 第二次（同 member）：幂等 → 同一 node/job，不新建
        let b = provision_for_member(&mut c, "t1", "m1", "u1", &opts).unwrap();
        assert_eq!(b["same_member"], json!(true));
        let nodes = list_nodes(&c, "t1").unwrap();
        assert_eq!(nodes.len(), 1, "重复 invite 只应有一个 node");
        assert_eq!(a["node_id"], b["node_id"]);
    }

    #[test]
    fn reuse_existing_host_for_same_user_other_team() {
        let mut c = conn();
        c.execute(
            "INSERT INTO teams (id,name,state,invite_token,created_at,updated_at) VALUES ('t1','T','active','tok','now','now')",
            [],
        ).unwrap();
        let opts = ProvisionOpts { region: Some("ap-east-1".into()), ..Default::default() };
        let a = provision_for_member(&mut c, "t1", "m1", "u1", &opts).unwrap();
        // 标记为 ready（可复用）
        set_node_state(&mut c, a["node_id"].as_str().unwrap(), "provisioning").ok();
        c.execute("UPDATE nodes SET state='ready' WHERE id=?1", params![a["node_id"].as_str().unwrap()]).unwrap();
        // 同 user 在另一 team → 应复用（不新建）
        let b = provision_for_member(&mut c, "t1", "m2", "u1", &opts).unwrap();
        assert_eq!(b["reused"], json!(true));
        assert_eq!(list_nodes(&c, "t1").unwrap().len(), 1);
    }

    #[test]
    fn destroy_is_idempotent_noop_on_second_call() {
        struct Noop;
        impl CloudProvider for Noop {
            fn name(&self) -> &str { "noop" }
            fn create(&self, _r: &CreateReq) -> Result<Created, String> { unimplemented!() }
            fn status(&self, _i: &str) -> Result<String, String> { Ok("running".into()) }
            fn destroy(&self, _i: &str) -> Result<(), String> { Ok(()) }
            fn list(&self) -> Result<Vec<String>, String> { Ok(vec![]) }
        }
        let mut c = conn();
        c.execute(
            "INSERT INTO teams (id,name,state,invite_token,created_at,updated_at) VALUES ('t1','T','active','tok','now','now')",
            [],
        ).unwrap();
        let opts = ProvisionOpts { region: Some("r".into()), ..Default::default() };
        let a = provision_for_member(&mut c, "t1", "m1", "u1", &opts).unwrap();
        let nid = a["node_id"].as_str().unwrap().to_string();
        c.execute("UPDATE nodes SET state='ready' WHERE id=?1", params![nid]).unwrap();
        let first = destroy_node(&mut c, &nid, &Noop).unwrap();
        assert_eq!(first["ok"], json!(true));
        assert!(first["checklist"].as_array().unwrap().len() >= 8);
        let second = destroy_node(&mut c, &nid, &Noop).unwrap();
        assert_eq!(second["noop"], json!(true));
    }

    #[test]
    fn reuse_inflight_host_for_same_user() {
        let mut c = conn();
        c.execute(
            "INSERT INTO teams (id,name,state,invite_token,created_at,updated_at) VALUES ('t1','T','active','tok','now','now')",
            [],
        ).unwrap();
        let opts = ProvisionOpts { region: Some("r".into()), ..Default::default() };
        let a = provision_for_member(&mut c, "t1", "m1", "u1", &opts).unwrap();
        // 第一台仍在 provisioning（未 ready）→ 同 user 另一 seat 也应复用，不新建
        let b = provision_for_member(&mut c, "t1", "m2", "u1", &opts).unwrap();
        assert_eq!(b["reused"], json!(true));
        assert_eq!(list_nodes(&c, "t1").unwrap().len(), 1);
        assert_eq!(a["node_id"], b["node_id"]);
    }

    #[test]
    fn bootstrap_script_contains_key_steps() {
        let s = bootstrap_script(
            "abc1234",
            "https://s:5783",
            "CA-PEM",
            "bGV0dGVy",
            "https://repo/x.git",
            "mid-2",
            "node-2",
        );
        for needle in [
            "OS_USER=abc1234",
            "useradd -m -s /bin/bash $OS_USER",
            "opencode.ai/install",
            "/usr/local/bin/teamx",
            "teamx team import",
            "teamx-$OS_USER-opencode-serve.service",
            "systemctl enable teamx-$OS_USER-",
            "BOOTSTRAP_OK",
        ] {
            assert!(s.contains(needle), "missing `{needle}` in bootstrap script");
        }
        // 服务只注册不启动（裁决 V）
        assert!(!s.contains("systemctl start"), "must not start services before approval");
    }

    #[test]
    fn verify_host_key_matches_console() {
        let console = "cloud-init: host key SHA256:abc123 done";
        let scan = "1.2.3.4 ssh-ed25519 AAAA... SHA256:abc123";
        assert!(verify_host_key(console, scan).unwrap());
        assert!(verify_host_key(console, "SHA256:zzz").is_err());
        // 无控制台数据 → 首登 TOFU（false，不报错）
        assert!(!verify_host_key("", scan).unwrap());
    }

    struct FakeProvider {
        fail: bool,
    }
    impl CloudProvider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }
        fn create(&self, _r: &CreateReq) -> Result<Created, String> {
            if self.fail {
                return Err("boom".into());
            }
            Ok(Created {
                instance_id: "i-1".into(),
                private_ip: Some("10.0.0.1".into()),
                public_ip: Some("1.2.3.4".into()),
            })
        }
        fn status(&self, _i: &str) -> Result<String, String> {
            Ok("running".into())
        }
        fn destroy(&self, _i: &str) -> Result<(), String> {
            Ok(())
        }
        fn list(&self) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
    }

    struct FakeExec;
    impl RemoteExec for FakeExec {
        fn run_as(&self, _ip: &str, _k: &str, _u: &str, script: &str) -> Result<String, String> {
            if script.contains("echo READY") {
                Ok("READY".into())
            } else if script.trim() == "uname -m" {
                Ok(std::env::consts::ARCH.into())
            } else {
                Ok("BOOTSTRAP_OK".into())
            }
        }
        fn upload(&self, _ip: &str, _k: &str, _l: &str, _r: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn seed(c: &mut Connection) -> Value {
        c.execute(
            "INSERT INTO teams (id,name,state,invite_token,created_at,updated_at) VALUES ('t1','T','active','tok','now','now')",
            [],
        ).unwrap();
        let opts = ProvisionOpts { region: Some("r".into()), ..Default::default() };
        provision_for_member(c, "t1", "m1", "u1", &opts).unwrap()
    }

    #[test]
    fn run_provision_job_happy_path() {
        let mut c = conn();
        let p = seed(&mut c);
        let job = p["job_id"].as_str().unwrap().to_string();
        let r = run_provision_job(
            &mut c, &job, &FakeProvider { fail: false }, &FakeExec, "/tmp/k",
            "https://s", "bGV0dGVy", "CA", "https://repo",
        ).unwrap();
        assert_eq!(r["state"], json!("ready"));
        let node = get_node(&c, p["node_id"].as_str().unwrap()).unwrap().unwrap();
        assert_eq!(node["state"], json!("ready"));
        assert_eq!(node["instance_id"], json!("i-1"));
        // job 完成
        let jstate: String = c.query_row("SELECT state FROM provision_jobs WHERE id=?1", params![job], |r| r.get(0)).unwrap();
        assert_eq!(jstate, "done");
    }

    #[test]
    fn run_provision_job_marks_failed_on_provider_error() {
        let mut c = conn();
        let p = seed(&mut c);
        let job = p["job_id"].as_str().unwrap().to_string();
        let e = run_provision_job(
            &mut c, &job, &FakeProvider { fail: true }, &FakeExec, "/tmp/k",
            "https://s", "bGV0dGVy", "CA", "https://repo",
        );
        assert!(e.is_err());
        let node = get_node(&c, p["node_id"].as_str().unwrap()).unwrap().unwrap();
        assert_eq!(node["state"], json!("failed")); // 裁决 U：失败标记，不自动 terminate
        let jstate: String = c.query_row("SELECT state FROM provision_jobs WHERE id=?1", params![job], |r| r.get(0)).unwrap();
        assert_eq!(jstate, "failed");
    }

    #[test]
    fn run_destroy_executes_checklist() {
        let mut c = conn();
        let p = seed(&mut c);
        let nid = p["node_id"].as_str().unwrap().to_string();
        c.execute("UPDATE nodes SET state='ready', instance_id='i-9', ip='1.2.3.4', keypair_ref='teamx-k' WHERE id=?1", params![nid]).unwrap();
        let r = run_destroy(&mut c, &nid, &FakeProvider { fail: false }, &FakeExec, "/tmp/k").unwrap();
        assert_eq!(r["state"], json!("terminated"));
        let steps = r["steps"].as_array().unwrap();
        assert!(steps.iter().any(|s| s["step"] == json!("terminate-instance")));
        assert!(steps.iter().any(|s| s["step"] == json!("delete-os-user") || s["step"] == json!("stop-services-and-delete-os-user")));
        assert!(steps.iter().any(|s| s["step"] == json!("delete-keypair")));
    }

    #[test]
    fn auto_stop_two_level() {
        let a = "a".to_string();
        let b = "b".to_string();
        // 全部 idle → 实例级全停 + 整机级可停
        let (inst, machine) = auto_stop_decision(&[(a.clone(), true), (b.clone(), true)]);
        assert_eq!(inst.len(), 2);
        assert!(machine);
        // 一个忙 → 只停 idle 实例，整机不停（避免误杀）
        let (inst, machine) = auto_stop_decision(&[(a.clone(), true), (b.clone(), false)]);
        assert_eq!(inst, vec![a.clone()]);
        assert!(!machine);
        // 空集合 → 不停机
        let (inst, machine) = auto_stop_decision(&[]);
        assert!(inst.is_empty());
        assert!(!machine);
    }

    #[test]
    fn region_falls_back_to_server_region() {
        let opts = ProvisionOpts { server_region: Some("ap-east-1".into()), ..Default::default() };
        assert_eq!(resolve_region(&opts).unwrap(), "ap-east-1");
        let explicit = ProvisionOpts { region: Some("us-west-2".into()), server_region: Some("ap-east-1".into()), ..Default::default() };
        assert_eq!(resolve_region(&explicit).unwrap(), "us-west-2");
        assert!(resolve_region(&ProvisionOpts::default()).is_err());
    }
}
