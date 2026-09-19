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
}

/// 建机结果。
#[derive(Debug, Clone)]
pub struct Created {
    pub instance_id: String,
    pub private_ip: Option<String>,
    pub public_ip: Option<String>,
}

/// 云 provider 抽象（design/006 §1：接口按多后端设计，AWS CLI 先行）。
pub trait CloudProvider {
    fn name(&self) -> &str;
    fn create(&self, req: &CreateReq) -> Result<Created, String>;
    fn status(&self, instance_id: &str) -> Result<String, String>;
    fn destroy(&self, instance_id: &str) -> Result<(), String>;
    fn list(&self) -> Result<Vec<String>, String>;
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
        let tags = req
            .tags
            .iter()
            .map(|(k, v)| format!("Key={k},Value={v}"))
            .collect::<Vec<_>>()
            .join(",");
        let txt = self.run(&[
            "ec2",
            "run-instances",
            "--image-id",
            &ami,
            "--instance-type",
            &req.size,
            "--key-name",
            &req.key_name,
            "--tag-specifications",
            &format!("ResourceType=instance,Tags=[{tags}]"),
            "--query",
            "Instances[0].[InstanceId,PrivateIpAddress,PublicIpAddress]",
        ])?;
        let v: Value = serde_json::from_str(&txt).map_err(|e| format!("bad aws json: {e}"))?;
        let arr = v.as_array().ok_or("unexpected aws response")?;
        let s = |i: usize| arr.get(i).and_then(Value::as_str).map(str::to_string);
        let instance_id = s(0).ok_or("aws: missing InstanceId")?;
        Ok(Created {
            instance_id,
            private_ip: s(1),
            public_ip: s(2),
        })
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

/// 按 provider 名构造实现。
pub fn provider_for(name: &str, region: &str) -> Result<Box<dyn CloudProvider>, String> {
    match name {
        "aws" => Ok(Box::new(AwsCli::new(region))),
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
    }))
}

const NODE_COLS: &str = "id,team_id,user_id,member_id,provider,region,instance_id,ip,state,\
                         im_enabled,os_user,keypair_ref,created_at,ready_at,terminated_at";

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
    conn.query_row(&sql, params![team_id, user_id], |r| node_json(r))
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
    conn.query_row(&sql, params![team_id, member_id], |r| node_json(r))
        .optional()
        .map_err(|e| format!("db: {e}"))
}

pub fn get_node(conn: &Connection, node_id: &str) -> Result<Option<Value>, String> {
    let sql = format!("SELECT {NODE_COLS} FROM nodes WHERE id = ?1");
    conn.query_row(&sql, params![node_id], |r| node_json(r))
        .optional()
        .map_err(|e| format!("db: {e}"))
}

pub fn list_nodes(conn: &Connection, team_id: &str) -> Result<Vec<Value>, String> {
    let sql = format!(
        "SELECT {NODE_COLS} FROM nodes WHERE team_id = ?1 ORDER BY created_at DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![team_id], |r| node_json(r))
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
            "job_id": job_id, "job_created": created,
            "note": "reuse existing host for this user (design/006 §3.1)",
        }));
    }

    // 2) 新建：落 node（requested）+ job（requested）。
    let node_id = uuid::Uuid::new_v4().to_string();
    let now = db::now();
    let os_user = os_user_for(team_id, &node_id);
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

/// 状态推进（落库 + 校验）。
pub fn set_node_state(conn: &mut Connection, node_id: &str, to: &str) -> Result<(), String> {
    let from: String = conn
        .query_row("SELECT state FROM nodes WHERE id = ?1", params![node_id], |r| r.get(0))
        .map_err(|e| format!("node {node_id} not found: {e}"))?;
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
    }

    #[test]
    fn os_user_is_slug_plus_4_digits() {
        let u = os_user_for("abc", "1111aaaa-2222-bbbb-cccc-ddddeeeeffff");
        assert!(u.starts_with("abc"), "{u}");
        assert_eq!(u.len(), 7, "{u}"); // abc + 1111
        assert!(u.chars().skip(3).all(|c| c.is_ascii_digit()), "{u}");
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
    fn region_falls_back_to_server_region() {
        let opts = ProvisionOpts { server_region: Some("ap-east-1".into()), ..Default::default() };
        assert_eq!(resolve_region(&opts).unwrap(), "ap-east-1");
        let explicit = ProvisionOpts { region: Some("us-west-2".into()), server_region: Some("ap-east-1".into()), ..Default::default() };
        assert_eq!(resolve_region(&explicit).unwrap(), "us-west-2");
        assert!(resolve_region(&ProvisionOpts::default()).is_err());
    }
}
