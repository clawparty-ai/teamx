//! 团队发现与加入（design/015 grill 定稿）。
//!
//! 范围（本模块只做平台侧）：
//! - 可见性 / 加入策略 / 公开简介的读写（`teams.visibility|join_policy|public_summary`）；
//! - 「全部团队」名录查询（**只读平台库**，不调用租户 server —— SD-9 / ADR-0001）；
//! - 加入申请（`team_join_requests`）：申请 / 撤回 / owner 审批 / 冷却。
//!
//! 不做（明确划出）：
//! - 详情取数（走 service 车道调租户 server + 服务端白名单裁剪）；
//! - 批准后的 seat/邀请函/异步 pod（复用既有 invite + provision job 路径）。

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// 可见性封闭枚举（SD-8）。
pub const VISIBILITIES: &[&str] = &["public", "private"];
/// 加入策略封闭枚举（SD-16，与可见性正交）。
pub const JOIN_POLICIES: &[&str] = &["open_request", "invite_only"];
/// 名录只列这两个状态（SD-8）。
pub const DIRECTORY_STATES: &[&str] = &["forming", "active"];
/// 被拒后的冷却天数（SD-11）。
pub const REJECT_COOLDOWN_DAYS: i64 = 7;

fn now() -> String {
    crate::db::now()
}

fn now_plus_days(days: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339()
}

/// 设置团队可见性（仅 owner 语义；调用方负责鉴权）。
pub fn set_visibility(conn: &Connection, team_id: &str, visibility: &str) -> Result<Value, String> {
    if !VISIBILITIES.contains(&visibility) {
        return Err(format!(
            "invalid visibility `{visibility}` (allowed: {})",
            VISIBILITIES.join("|")
        ));
    }
    let n = conn
        .execute(
            "UPDATE teams SET visibility = ?1, updated_at = ?2 WHERE id = ?3 AND state != 'destroyed'",
            params![visibility, now(), team_id],
        )
        .map_err(|e| format!("db: {e}"))?;
    if n == 0 {
        return Err(format!("team {team_id} not found"));
    }
    Ok(json!({ "ok": true, "team_id": team_id, "visibility": visibility }))
}

/// 设置加入策略（SD-16）。
pub fn set_join_policy(conn: &Connection, team_id: &str, policy: &str) -> Result<Value, String> {
    if !JOIN_POLICIES.contains(&policy) {
        return Err(format!(
            "invalid join_policy `{policy}` (allowed: {})",
            JOIN_POLICIES.join("|")
        ));
    }
    let n = conn
        .execute(
            "UPDATE teams SET join_policy = ?1, updated_at = ?2 WHERE id = ?3 AND state != 'destroyed'",
            params![policy, now(), team_id],
        )
        .map_err(|e| format!("db: {e}"))?;
    if n == 0 {
        return Err(format!("team {team_id} not found"));
    }
    Ok(json!({ "ok": true, "team_id": team_id, "join_policy": policy }))
}

/// 设置公开简介（详情白名单字段，SD-15）。限长 200 字（按字符计）。
pub fn set_public_summary(conn: &Connection, team_id: &str, summary: &str) -> Result<Value, String> {
    let s = summary.trim();
    if s.chars().count() > 200 {
        return Err("public_summary must be <= 200 characters".to_string());
    }
    let n = conn
        .execute(
            "UPDATE teams SET public_summary = ?1, updated_at = ?2 WHERE id = ?3 AND state != 'destroyed'",
            params![s, now(), team_id],
        )
        .map_err(|e| format!("db: {e}"))?;
    if n == 0 {
        return Err(format!("team {team_id} not found"));
    }
    Ok(json!({ "ok": true, "team_id": team_id }))
}

/// 团队发现设置（owner 视角：含策略）与公开视图（详情用：不含策略细节）。
pub fn settings(conn: &Connection, team_id: &str) -> Result<Option<Value>, String> {
    conn.query_row(
        "SELECT id, name, COALESCE(visibility,'private'), COALESCE(join_policy,'invite_only'),
                COALESCE(public_summary,''), COALESCE(state,'')
         FROM teams WHERE id = ?1",
        params![team_id],
        |r| {
            Ok(json!({
                "team_id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "visibility": r.get::<_, String>(2)?,
                "join_policy": r.get::<_, String>(3)?,
                "public_summary": r.get::<_, String>(4)?,
                "state": r.get::<_, String>(5)?,
            }))
        },
    )
    .optional()
    .map_err(|e| format!("db: {e}"))
}

/// 「全部团队」名录（SD-19/SD-25）：搜索 + 排序 + 筛选 + 分页。
///
/// **只读平台库**（ADR-0001）。排序键仅限平台库可得字段：`created_at` | `members`（成员数快照，
/// 由 `node`/`tenants` 侧计数得来，取 `members` 表活跃座位数）。
pub fn directory(
    conn: &Connection,
    query: Option<&str>,
    sort: Option<&str>,
    joinable_only: bool,
    page: i64,
    per_page: i64,
) -> Result<Value, String> {
    let per_page = per_page.clamp(1, 100);
    let page = page.max(1);
    let offset = (page - 1) * per_page;
    let like = format!("%{}%", query.unwrap_or("").trim());
    // 排序白名单（SQL 拼接仅用常量，杜绝注入）
    let order = match sort.unwrap_or("created") {
        "members" => "member_count DESC, t.created_at DESC",
        _ => "t.created_at DESC",
    };
    let sql = format!(
        "SELECT t.id, t.name, COALESCE(t.public_summary,''), COALESCE(t.join_policy,'invite_only'),
                t.created_at, t.updated_at,
                (SELECT COUNT(*) FROM members m WHERE m.team_id = t.id AND m.state NOT IN ('left','denied'))
         FROM teams t
         WHERE COALESCE(t.visibility,'private') = 'public'
           AND t.state IN ('forming','active')
           AND (?1 = '%%' OR t.name LIKE ?1 OR COALESCE(t.public_summary,'') LIKE ?1)
           AND (?2 = 0 OR COALESCE(t.join_policy,'invite_only') = 'open_request')
         ORDER BY {order}
         LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![like, joinable_only as i64, per_page, offset], |r| {
            let join_policy: String = r.get(3)?;
            Ok(json!({
                "team_id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "public_summary": r.get::<_, String>(2)?,
                "joinable": join_policy == "open_request",
                "created_at": r.get::<_, String>(4)?,
                "updated_at": r.get::<_, String>(5)?,
                "member_count": r.get::<_, i64>(6)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
    let items: Vec<Value> = rows.collect::<rusqlite::Result<_>>().map_err(|e| format!("db: {e}"))?;
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM teams t
             WHERE COALESCE(t.visibility,'private') = 'public'
               AND t.state IN ('forming','active')
               AND (?1 = '%%' OR t.name LIKE ?1 OR COALESCE(t.public_summary,'') LIKE ?1)
               AND (?2 = 0 OR COALESCE(t.join_policy,'invite_only') = 'open_request')",
            params![like, joinable_only as i64],
            |r| r.get(0),
        )
        .map_err(|e| format!("db: {e}"))?;
    Ok(json!({
        "ok": true, "items": items, "page": page, "per_page": per_page, "total": total,
    }))
}

/// 申请人视角的可见性检查（SD-20：private 完全不可见 → 403）。
pub fn visible_to(conn: &Connection, team_id: &str, viewer_user_id: Option<&str>) -> Result<bool, String> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT COALESCE(visibility,'private'), COALESCE(state,''), id FROM teams WHERE id = ?1",
            params![team_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    let Some((visibility, state, _)) = row else {
        return Ok(false);
    };
    if state == "destroyed" {
        return Ok(false);
    }
    if visibility == "public" {
        return Ok(true);
    }
    // private：仅归属用户可见
    match viewer_user_id {
        Some(uid) => crate::platform::tenant_owned_by(conn, team_id, uid),
        None => Ok(false),
    }
}

/// 发起加入申请（SD-11）。
///
/// 前置：团队 public + `join_policy=open_request` + 非成员 + 无 pending + 不在冷却期。
pub fn request_join(
    conn: &mut Connection,
    team_id: &str,
    user_id: &str,
    message: Option<&str>,
) -> Result<Value, String> {
    let Some(s) = settings(conn, team_id)? else {
        return Err(format!("team {team_id} not found"));
    };
    if s["visibility"] != json!("public") || s["join_policy"] != json!("open_request") {
        return Err("this team is not open for join requests".to_string());
    }
    // 已是成员？（平台侧只能看归属；成员资格在租户 server，这里以归属 + 既有申请近似）
    if crate::platform::tenant_owned_by(conn, team_id, user_id)? {
        return Err("you already own this team".to_string());
    }
    let pending: Option<String> = conn
        .query_row(
            "SELECT id FROM team_join_requests WHERE team_id = ?1 AND user_id = ?2 AND status = 'pending'",
            params![team_id, user_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    if pending.is_some() {
        return Err("you already have a pending request for this team".to_string());
    }
    // 冷却期
    let cd: Option<String> = conn
        .query_row(
            "SELECT cooldown_until FROM team_join_requests
             WHERE team_id = ?1 AND user_id = ?2 AND status = 'rejected' AND cooldown_until IS NOT NULL
             ORDER BY decided_at DESC LIMIT 1",
            params![team_id, user_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?
        .flatten();
    if let Some(until) = cd {
        if until.as_str() > now().as_str() {
            return Err(format!("rejected recently; you may re-apply after {until}"));
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO team_join_requests (id, team_id, user_id, status, message, created_at)
         VALUES (?1, ?2, ?3, 'pending', ?4, ?5)",
        params![id, team_id, user_id, message.map(str::trim).filter(|m| !m.is_empty()), now()],
    )
    .map_err(|e| format!("db: {e}"))?;
    Ok(json!({ "ok": true, "request_id": id, "status": "pending" }))
}

/// 申请人撤回（SD-26）。
pub fn withdraw_request(conn: &Connection, request_id: &str, user_id: &str) -> Result<Value, String> {
    let n = conn
        .execute(
            "UPDATE team_join_requests SET status = 'withdrawn', decided_at = ?1
             WHERE id = ?2 AND user_id = ?3 AND status = 'pending'",
            params![now(), request_id, user_id],
        )
        .map_err(|e| format!("db: {e}"))?;
    if n == 0 {
        return Err("no pending request with that id for you".to_string());
    }
    Ok(json!({ "ok": true, "request_id": request_id, "status": "withdrawn" }))
}

/// owner 审批（SD-11：approve / reject）。`approve=true` 时返回申请人信息，
/// 供调用方继续执行"建 seat + 发函 + 异步拉 pod"（本函数不碰租户 server）。
pub fn decide_request(
    conn: &mut Connection,
    request_id: &str,
    decider_user_id: &str,
    approve: bool,
) -> Result<Value, String> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT team_id, user_id, status FROM team_join_requests WHERE id = ?1",
            params![request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    let (team_id, applicant, status) = row.ok_or_else(|| "request not found".to_string())?;
    if status != "pending" {
        return Err(format!("request is already {status}"));
    }
    // 鉴权：decider 必须是该团队的归属用户（owner 语义）
    if !crate::platform::tenant_owned_by(conn, &team_id, decider_user_id)? {
        return Err("only the team owner may decide join requests".to_string());
    }
    let (new_status, cooldown): (&str, Option<String>) = if approve {
        ("approved", None)
    } else {
        ("rejected", Some(now_plus_days(REJECT_COOLDOWN_DAYS)))
    };
    conn.execute(
        "UPDATE team_join_requests SET status = ?1, decided_at = ?2, decided_by = ?3, cooldown_until = ?4
         WHERE id = ?5",
        params![new_status, now(), decider_user_id, cooldown, request_id],
    )
    .map_err(|e| format!("db: {e}"))?;
    let name: String = conn
        .query_row(
            "SELECT display_name FROM platform_users WHERE id = ?1",
            params![applicant],
            |r| r.get(0),
        )
        .unwrap_or_default();
    Ok(json!({
        "ok": true, "request_id": request_id, "status": new_status,
        "team_id": team_id, "applicant_user_id": applicant, "applicant_name": name,
    }))
}

/// 记录批准后的交付结果（SD-11/SD-23）：座位 id、邀请函、拉 pod 状态。
pub fn record_delivery(
    conn: &Connection,
    request_id: &str,
    member_id: &str,
    letter: &str,
    pod_state: &str,
    pod_detail: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE team_join_requests SET member_id = ?1, letter = ?2, pod_state = ?3, pod_detail = ?4
         WHERE id = ?5",
        params![member_id, letter, pod_state, pod_detail, request_id],
    )
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// 「我的申请」（SD-26）：状态 + 冷却；approved 行带**邀请函**与 pod 状态。
pub fn my_requests(conn: &Connection, user_id: &str) -> Result<Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT r.id, r.team_id, t.name, r.status, r.created_at, r.decided_at, r.cooldown_until,
                    COALESCE(r.member_id,''), COALESCE(r.letter,''), COALESCE(r.pod_state,''), COALESCE(r.pod_detail,'')
             FROM team_join_requests r LEFT JOIN teams t ON t.id = r.team_id
             WHERE r.user_id = ?1 ORDER BY r.created_at DESC",
        )
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![user_id], |r| {
            Ok(json!({
                "request_id": r.get::<_, String>(0)?,
                "team_id": r.get::<_, String>(1)?,
                "team_name": r.get::<_, Option<String>>(2)?,
                "status": r.get::<_, String>(3)?,
                "created_at": r.get::<_, String>(4)?,
                "decided_at": r.get::<_, Option<String>>(5)?,
                "cooldown_until": r.get::<_, Option<String>>(6)?,
                "member_id": r.get::<_, String>(7)?,
                "letter": r.get::<_, String>(8)?,
                "pod_state": r.get::<_, String>(9)?,
                "pod_detail": r.get::<_, String>(10)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
    let items: Vec<Value> = rows.collect::<rusqlite::Result<_>>().map_err(|e| format!("db: {e}"))?;
    Ok(json!({ "ok": true, "requests": items }))
}

/// owner 视角：我名下团队的待审申请。
pub fn pending_for_owner(conn: &Connection, owner_user_id: &str) -> Result<Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT r.id, r.team_id, t.name, r.user_id, u.display_name, r.message, r.created_at
             FROM team_join_requests r
             JOIN tenant_owners o ON o.team_id = r.team_id AND o.user_id = ?1
             LEFT JOIN teams t ON t.id = r.team_id
             LEFT JOIN platform_users u ON u.id = r.user_id
             WHERE r.status = 'pending' ORDER BY r.created_at",
        )
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![owner_user_id], |r| {
            Ok(json!({
                "request_id": r.get::<_, String>(0)?,
                "team_id": r.get::<_, String>(1)?,
                "team_name": r.get::<_, Option<String>>(2)?,
                "applicant_user_id": r.get::<_, String>(3)?,
                "applicant_name": r.get::<_, Option<String>>(4)?,
                "message": r.get::<_, Option<String>>(5)?,
                "created_at": r.get::<_, String>(6)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
    let items: Vec<Value> = rows.collect::<rusqlite::Result<_>>().map_err(|e| format!("db: {e}"))?;
    Ok(json!({ "ok": true, "requests": items }))
}

/// 可见性**投影到 k8s namespace label**（design/015 SD-14 / SD-24）。
///
/// 平台库是唯一事实源；label 是投影。幂等：只在值不一致时打标，成功后记录
/// `tenants.visibility_projected`。失败**不抛给调用方**（写库已成功），返回 `projected:false`
/// 由对账补偿。
pub fn project_visibility(
    conn: &Connection,
    team_id: &str,
    kubeconfig: Option<&str>,
    context: Option<&str>,
) -> Result<Value, String> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT COALESCE(t.visibility,'private'), COALESCE(n.namespace,''), COALESCE(n.visibility_projected,'')
             FROM teams t LEFT JOIN tenants n ON n.team_id = t.team_id WHERE t.id = ?1",
            params![team_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    let Some((visibility, ns, projected)) = row else {
        return Ok(json!({ "ok": false, "reason": "team not found" }));
    };
    if ns.is_empty() {
        return Ok(json!({ "ok": false, "reason": "no tenant namespace yet" }));
    }
    if projected == visibility {
        return Ok(json!({ "ok": true, "projected": true, "unchanged": true, "visibility": visibility }));
    }
    let kubectl = crate::k8s::Kubectl::new(
        kubeconfig.map(str::to_string),
        context.map(str::to_string),
    );
    match kubectl.label("namespace", &ns, "teamx.io/visibility", &visibility, "default") {
        Ok(()) => {
            conn.execute(
                "UPDATE tenants SET visibility_projected = ?1 WHERE team_id = ?2",
                params![visibility, team_id],
            )
            .map_err(|e| format!("db: {e}"))?;
            Ok(json!({ "ok": true, "projected": true, "visibility": visibility, "namespace": ns }))
        }
        Err(e) => Ok(json!({
            "ok": false, "projected": false, "visibility": visibility, "namespace": ns,
            "reason": format!("kubectl label failed: {e}"),
        })),
    }
}

/// 对账（SD-24）：把**所有**与平台库不一致的租户 label 修回；返回逐条结果。
pub fn reconcile_visibility(
    conn: &Connection,
    kubeconfig: Option<&str>,
    context: Option<&str>,
) -> Result<Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT n.team_id, COALESCE(t.visibility,'private'), COALESCE(n.visibility_projected,'')
             FROM tenants n LEFT JOIN teams t ON t.id = n.team_id",
        )
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .map_err(|e| format!("db: {e}"))?;
    let pending: Vec<(String, String, String)> = rows
        .collect::<rusqlite::Result<Vec<(String, String, String)>>>()
        .map_err(|e| format!("db: {e}"))?
        .into_iter()
        .filter(|(_, vis, proj)| vis != proj)
        .collect();
    let mut fixed = Vec::new();
    let mut failed = Vec::new();
    for (team_id, _, _) in &pending {
        let r = project_visibility(conn, team_id, kubeconfig, context)?;
        if r["projected"] == json!(true) {
            fixed.push(json!({ "team_id": team_id, "visibility": r["visibility"] }));
        } else {
            failed.push(json!({ "team_id": team_id, "reason": r["reason"] }));
        }
    }
    Ok(json!({
        "ok": true, "checked": pending.len(), "pending": pending.len(),
        "fixed": fixed, "failed": failed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        c
    }

    fn seed_team(c: &Connection, id: &str, visibility: &str, policy: &str) {
        c.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at, visibility, join_policy)
             VALUES (?1, 'T', NULL, NULL, 'active', 'tok', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00', ?2, ?3)",
            params![id, visibility, policy],
        ).unwrap();
    }

    fn seed_user(c: &mut Connection, id: &str, name: &str) {
        crate::platform::create_user(c, name, "password-1").unwrap();
        c.execute("UPDATE platform_users SET id = ?1 WHERE display_name = ?2", params![id, name]).unwrap();
    }

    #[test]
    fn visibility_and_policy_are_closed_enums_with_safe_defaults() {
        let c = db();
        seed_team(&c, "t1", "private", "invite_only");
        // 默认值正确（SD-8/SD-16）
        let s = settings(&c, "t1").unwrap().unwrap();
        assert_eq!(s["visibility"], json!("private"));
        assert_eq!(s["join_policy"], json!("invite_only"));
        // 封闭枚举
        assert!(set_visibility(&c, "t1", "internal").is_err());
        assert!(set_join_policy(&c, "t1", "auto_approve").is_err());
        assert!(set_visibility(&c, "t1", "public").is_ok());
        assert!(set_join_policy(&c, "t1", "open_request").is_ok());
        assert_eq!(settings(&c, "t1").unwrap().unwrap()["visibility"], json!("public"));
        // 公开简介限长 200
        assert!(set_public_summary(&c, "t1", &"x".repeat(201)).is_err());
        assert!(set_public_summary(&c, "t1", "hello").is_ok());
    }

    #[test]
    fn directory_lists_only_public_active_with_search_sort_filter() {
        let c = db();
        seed_team(&c, "pub1", "public", "open_request");
        seed_team(&c, "pub2", "public", "invite_only");
        seed_team(&c, "priv", "private", "open_request");
        c.execute("UPDATE teams SET name = 'alpha' WHERE id = 'pub1'", []).unwrap();
        c.execute("UPDATE teams SET name = 'beta' WHERE id = 'pub2'", []).unwrap();
        c.execute("UPDATE teams SET state = 'completed' WHERE id = 'pub2'", []).unwrap();
        let d = directory(&c, None, None, false, 1, 20).unwrap();
        assert_eq!(d["total"], json!(1), "只列 public+active：{d}");
        assert_eq!(d["items"][0]["team_id"], json!("pub1"));
        assert_eq!(d["items"][0]["joinable"], json!(true), "open_request → joinable");
        // 搜索（名称）
        assert_eq!(directory(&c, Some("alph"), None, false, 1, 20).unwrap()["total"], json!(1));
        assert_eq!(directory(&c, Some("zzz"), None, false, 1, 20).unwrap()["total"], json!(0));
        // 筛选"可加入"
        c.execute("UPDATE teams SET state = 'active' WHERE id = 'pub2'", []).unwrap();
        assert_eq!(directory(&c, None, None, false, 1, 20).unwrap()["total"], json!(2));
        assert_eq!(directory(&c, None, None, true, 1, 20).unwrap()["total"], json!(1));
        // 分页
        let p = directory(&c, None, None, false, 2, 1).unwrap();
        assert_eq!(p["items"].as_array().unwrap().len(), 1);
        assert_eq!(p["page"], json!(2));
    }

    #[test]
    fn private_is_invisible_but_owner_visible() {
        let mut c = db();
        seed_user(&mut c, "u-owner", "owner1");
        seed_team(&c, "p1", "private", "invite_only");
        seed_team(&c, "q1", "public", "invite_only");
        assert!(!visible_to(&c, "p1", None).unwrap(), "private 对匿名不可见");
        assert!(!visible_to(&c, "p1", Some("u-other")).unwrap(), "private 对他人不可见");
        assert!(visible_to(&c, "q1", None).unwrap(), "public 可见");
        // 归属后 owner 可见
        crate::platform::tag_tenant_owner(&mut c, "p1", "u-owner").unwrap();
        assert!(visible_to(&c, "p1", Some("u-owner")).unwrap(), "owner 可见自己的 private");
    }

    #[test]
    fn join_request_flow_with_pending_guard_cooldown_and_owner_decision() {
        let mut c = db();
        seed_user(&mut c, "u-owner", "own");
        seed_user(&mut c, "u-app", "applicant");
        seed_team(&c, "t-open", "public", "open_request");
        seed_team(&c, "t-invite", "public", "invite_only");
        crate::platform::tag_tenant_owner(&mut c, "t-open", "u-owner").unwrap();

        // invite_only 不能申请（SD-16）
        assert!(request_join(&mut c, "t-invite", "u-app", None).is_err());
        // owner 自己不能申请自己的团队
        assert!(request_join(&mut c, "t-open", "u-owner", None).is_err());
        // 正常申请
        let r = request_join(&mut c, "t-open", "u-app", Some("让我加入")).unwrap();
        let rid = r["request_id"].as_str().unwrap().to_string();
        assert_eq!(r["status"], json!("pending"));
        // 重复申请被拒（部分唯一索引 + 前置检查）
        assert!(request_join(&mut c, "t-open", "u-app", None).is_err());
        // 非 owner 不能审批
        assert!(decide_request(&mut c, &rid, "u-app", true).is_err());
        // owner 待审列表
        let pend = pending_for_owner(&c, "u-owner").unwrap();
        assert_eq!(pend["requests"].as_array().unwrap().len(), 1);
        // 拒绝 → 冷却
        let d = decide_request(&mut c, &rid, "u-owner", false).unwrap();
        assert_eq!(d["status"], json!("rejected"));
        let cd: Option<String> = c
            .query_row("SELECT cooldown_until FROM team_join_requests WHERE id = ?1", params![rid], |r| r.get(0))
            .unwrap();
        assert!(cd.is_some(), "拒绝必须写冷却期（SD-11）");        let again = request_join(&mut c, "t-open", "u-app", None);
        assert!(again.is_err(), "冷却期内不能再次申请: {again:?}");
        // 冷却期过后可再申请（直接改库绕过时间）
        c.execute("UPDATE team_join_requests SET cooldown_until = '2000-01-01T00:00:00+00:00' WHERE id = ?1", params![rid]).unwrap();
        let r2 = request_join(&mut c, "t-open", "u-app", None).unwrap();
        let rid2 = r2["request_id"].as_str().unwrap().to_string();
        // 撤回
        let w = withdraw_request(&c, &rid2, "u-app").unwrap();
        assert_eq!(w["status"], json!("withdrawn"));
        // 再次申请后 owner 批准
        let r3 = request_join(&mut c, "t-open", "u-app", None).unwrap();
        let rid3 = r3["request_id"].as_str().unwrap().to_string();
        let a = decide_request(&mut c, &rid3, "u-owner", true).unwrap();
        assert_eq!(a["status"], json!("approved"));
        assert_eq!(a["applicant_user_id"], json!("u-app"));
        assert_eq!(a["applicant_name"], json!("applicant"));
        // 已决不能再决
        assert!(decide_request(&mut c, &rid3, "u-owner", true).is_err());
        // 我的申请可见全部历史
        let mine = my_requests(&c, "u-app").unwrap();
        assert!(mine["requests"].as_array().unwrap().len() >= 3);
        // 交付记录（座位/信/pod 状态）
        record_delivery(&c, &rid3, "m-123", "teamx-inv:v1:AAA", "running", "ok").unwrap();
        let m2 = my_requests(&c, "u-app").unwrap();
        let row = m2["requests"].as_array().unwrap().iter()
            .find(|x| x["request_id"] == json!(rid3)).unwrap();
        assert_eq!(row["member_id"], json!("m-123"));
        assert_eq!(row["letter"], json!("teamx-inv:v1:AAA"));
        assert_eq!(row["pod_state"], json!("running"));
    }
}
