//! `teamx cloud` (design/023): unified operations for the teamx cloud platform,
//! its tenants, and AWS nodes. One entry point so operators no longer need to
//! hand-write sqlite / kubectl / console HTTP / AWS CLI calls.
//!
//! Scope in this build: P0 auth (login/logout/whoami/status, token in
//! `~/.teamx/cloud.json`); P1/P2 user/team/tenant/member/join (read plus
//! additive writes); `node` and `bootstrap` delegate to existing handlers.
//! Kubernetes-heavy ops (tenant start/stop/status, ca import) land in a later
//! slice and return an explicit "not implemented yet" error for now.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::cli::{CloudBootstrapCmd, CloudCaCmd, CloudCmd, CloudJoinCmd, CloudMemberCmd, CloudTeamCmd, CloudTenantCmd, CloudUserCmd};

/// Resolve the cloud base URL: explicit > $TEAMX_CLOUD_URL > https://teamx.beer.
fn cloud_url(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| std::env::var("TEAMX_CLOUD_URL").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "https://teamx.beer".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn cloud_config_path() -> PathBuf {
    crate::db::teamx_home().join("cloud.json")
}

fn load_config() -> Value {
    std::fs::read_to_string(cloud_config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({ "servers": {} }))
}

fn save_config(v: &Value) -> Result<(), String> {
    let p = cloud_config_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir: {e}"))?;
    }
    let body = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(&p, body).map_err(|e| format!("write {}: {e}", p.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn session_for(url: &str) -> Option<Value> {
    load_config().get("servers")?.get(url).cloned()
}

/// Split a `name-or-id` and resolve to `(id, display_name)` in platform_users.
fn resolve_user(conn: &Connection, token: &str) -> Result<(String, String), String> {
    let t = token.trim();
    let row = conn.query_row(
        "SELECT id, display_name FROM platform_users WHERE id = ?1 OR display_name = ?1 LIMIT 1",
        params![t],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    );
    row.map_err(|_| format!("platform user `{t}` not found"))
}

fn resolve_team_id(conn: &Connection, team: &str) -> Result<String, String> {
    let t = team.trim();
    conn.query_row(
        "SELECT id FROM teams WHERE id = ?1 OR name = ?1 LIMIT 1",
        params![t],
        |r| r.get::<_, String>(0),
    )
    .map_err(|_| format!("team `{t}` not found"))
}

/// Login via `POST /api/login/password`, capture the session cookie.
fn do_login(url: &str, name: &str, password: &str) -> Result<Value, String> {
    let resp = ureq::post(&format!("{url}/api/login/password"))
        .set("Content-Type", "application/json")
        .send_json(json!({ "name": name, "password": password }))
        .map_err(|e| format!("login request failed: {e}"))?;
    let cookie = resp
        .header("set-cookie")
        .and_then(|c| c.split(';').next())
        .and_then(|kv| kv.split_once('=').map(|(_, v)| v.to_string()))
        .ok_or_else(|| "login: no session cookie returned".to_string())?;
    let body: Value = resp.into_json().map_err(|e| format!("login: bad response: {e}"))?;
    if !body.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Err(format!("login failed: {body}"));
    }
    Ok(json!({ "token": cookie, "display_name": body.get("display_name").cloned().unwrap_or(Value::Null) }))
}

pub fn run(conn: &mut Connection, cmd: &CloudCmd) -> Result<Value, String> {
    match cmd {
        CloudCmd::Login { server, name, password, token } => {
            let url = cloud_url(server.as_deref());
            if let Some(t) = token {
                let mut cfg = load_config();
                cfg["servers"][&url] = json!({ "token": t });
                save_config(&cfg)?;
                return Ok(json!({ "ok": true, "server": url, "note": "token stored" }));
            }
            let name = name.clone().ok_or_else(|| "login requires --name <user>".to_string())?;
            let password = password
                .clone()
                .or_else(|| std::env::var("TEAMX_CLOUD_PASSWORD").ok())
                .ok_or_else(|| "login requires --password (or $TEAMX_CLOUD_PASSWORD)".to_string())?;
            let out = do_login(&url, &name, &password)?;
            let mut cfg = load_config();
            cfg["servers"][&url] = json!({
                "token": out["token"], "display_name": out["display_name"], "user_name": name,
            });
            save_config(&cfg)?;
            Ok(json!({ "ok": true, "server": url, "display_name": out["display_name"] }))
        }
        CloudCmd::Logout { server, all } => {
            let mut cfg = load_config();
            if *all {
                cfg["servers"] = json!({});
            } else {
                let url = cloud_url(server.as_deref());
                if let Some(m) = cfg["servers"].as_object_mut() {
                    m.remove(&url);
                }
            }
            save_config(&cfg)?;
            Ok(json!({ "ok": true }))
        }
        CloudCmd::Whoami { server } => {
            let url = cloud_url(server.as_deref());
            let s = session_for(&url).ok_or_else(|| format!("not logged in to {url}; run `teamx cloud login --server {url} --name <user>`"))?;
            Ok(json!({ "ok": true, "server": url, "display_name": s.get("display_name"), "user_name": s.get("user_name") }))
        }
        CloudCmd::Status { server } => {
            let url = cloud_url(server.as_deref());
            let logged_in = session_for(&url).is_some();
            let teams: i64 = conn
                .query_row("SELECT COUNT(*) FROM teams WHERE state <> 'destroyed'", [], |r| r.get(0))
                .unwrap_or(0);
            let tenants: i64 = conn.query_row("SELECT COUNT(*) FROM tenants", [], |r| r.get(0)).unwrap_or(0);
            Ok(json!({ "ok": true, "server": url, "logged_in": logged_in, "teams": teams, "tenants": tenants }))
        }

        CloudCmd::User(u) => run_user(conn, u),
        CloudCmd::Team(t) => run_team(conn, t),
        CloudCmd::Tenant(t) => run_tenant(conn, t),
        CloudCmd::Join(j) => run_join(conn, j),
        CloudCmd::Node(n) => crate::commands::cmd_node(conn, n).map_err(|e| e.to_string()),
        CloudCmd::Bootstrap(b) => run_bootstrap(conn, b),
    }
}

fn run_user(conn: &mut Connection, cmd: &CloudUserCmd) -> Result<Value, String> {
    match cmd {
        CloudUserCmd::List => crate::platform::list_users(conn),
        CloudUserCmd::Create { name, password, quota } => {
            let out = crate::platform::create_user(conn, name, password)?;
            let uid = out.get("user_id").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(q) = quota {
                crate::platform::set_quota(conn, &uid, *q as i64)?;
            }
            Ok(out)
        }
        CloudUserCmd::Passwd { user, password } => {
            let (uid, _) = resolve_user(conn, user)?;
            crate::platform::set_password(conn, &uid, password)
        }
        CloudUserCmd::Disable { user } => {
            let (uid, _) = resolve_user(conn, user)?;
            crate::platform::disable_user(conn, &uid)
        }
        CloudUserCmd::Enable { user } => {
            let (uid, _) = resolve_user(conn, user)?;
            conn.execute("UPDATE platform_users SET disabled_at = NULL WHERE id = ?1", params![uid])
                .map_err(|e| format!("db: {e}"))?;
            Ok(json!({ "ok": true, "user_id": uid, "enabled": true }))
        }
        CloudUserCmd::Quota { user, n } => {
            let (uid, _) = resolve_user(conn, user)?;
            crate::platform::set_quota(conn, &uid, *n as i64)
        }
    }
}

fn run_team(conn: &mut Connection, cmd: &CloudTeamCmd) -> Result<Value, String> {
    match cmd {
        CloudTeamCmd::List { state } => {
            let mut sql = String::from("SELECT id,name,state,created_at FROM teams");
            if state.is_some() {
                sql.push_str(" WHERE state = ?1");
            }
            sql.push_str(" ORDER BY created_at");
            let mut stmt = conn.prepare(&sql).map_err(|e| format!("db: {e}"))?;
            let rows: Vec<Value> = if let Some(st) = state {
                stmt.query_map(params![st], |r| {
                    Ok(json!({ "team_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "state": r.get::<_, String>(2)? }))
                })
                .map_err(|e| format!("db: {e}"))?
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| format!("db: {e}"))?
            } else {
                stmt.query_map([], |r| {
                    Ok(json!({ "team_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "state": r.get::<_, String>(2)? }))
                })
                .map_err(|e| format!("db: {e}"))?
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| format!("db: {e}"))?
            };
            Ok(json!({ "ok": true, "teams": rows }))
        }
        CloudTeamCmd::Show { team } => {
            let tid = resolve_team_id(conn, team)?;
            let t = conn
                .query_row(
                    "SELECT id,name,state,owner_member_id FROM teams WHERE id = ?1",
                    params![tid],
                    |r| Ok(json!({ "team_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "state": r.get::<_, String>(2)?, "owner_member_id": r.get::<_, Option<String>>(3)? })),
                )
                .map_err(|e| format!("db: {e}"))?;
            let tenant = crate::provision::tenant_get(conn, &tid).unwrap_or(None);
            Ok(json!({ "ok": true, "team": t, "tenant": tenant }))
        }
        CloudTeamCmd::Create { name, owner, visibility, join_policy } => {
            let exists: i64 = conn
                .query_row("SELECT COUNT(*) FROM teams WHERE name = ?1", params![name], |r| r.get(0))
                .map_err(|e| format!("db: {e}"))?;
            if exists > 0 {
                return Err(format!("team `{name}` already exists"));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let now = crate::db::now();
            let vis = visibility.clone().unwrap_or_else(|| "private".into());
            let jp = join_policy.clone().unwrap_or_else(|| "invite_only".into());
            if !matches!(vis.as_str(), "private" | "public") {
                return Err("visibility must be private|public".into());
            }
            if !matches!(jp.as_str(), "invite_only" | "open_request") {
                return Err("join_policy must be invite_only|open_request".into());
            }
            let token = crate::util::gen_access_token(16);
            conn.execute(
                "INSERT INTO teams (id,name,owner_member_id,goal_id,state,invite_token,created_at,updated_at,visibility,join_policy)
                 VALUES (?1,?2,NULL,NULL,'forming',?3,?4,?4,?5,?6)",
                params![id, name, token, now, vis, jp],
            )
            .map_err(|e| format!("db: {e}"))?;
            if let Some(u) = owner {
                let (uid, _) = resolve_user(conn, u)?;
                crate::platform::tag_tenant_owner(conn, &id, &uid)?;
            }
            Ok(json!({ "ok": true, "team_id": id, "name": name, "visibility": vis, "join_policy": jp }))
        }
        CloudTeamCmd::Rename { team, new_name } => {
            let tid = resolve_team_id(conn, team)?;
            conn.execute("UPDATE teams SET name = ?1, updated_at = ?2 WHERE id = ?3", params![new_name, crate::db::now(), tid])
                .map_err(|e| format!("db: {e}"))?;
            Ok(json!({ "ok": true, "team_id": tid, "name": new_name }))
        }
        CloudTeamCmd::SetOwner { team, user } => {
            let tid = resolve_team_id(conn, team)?;
            let (uid, _) = resolve_user(conn, user)?;
            conn.execute("DELETE FROM tenant_owners WHERE team_id = ?1", params![tid]).map_err(|e| format!("db: {e}"))?;
            crate::platform::tag_tenant_owner(conn, &tid, &uid)?;
            Ok(json!({ "ok": true, "team_id": tid, "owner_user_id": uid }))
        }
        CloudTeamCmd::Destroy { team, yes } => {
            let tid = resolve_team_id(conn, team)?;
            if !*yes {
                return Err(format!("refusing to delete team {tid} without --yes"));
            }
            conn.execute("DELETE FROM team_join_requests WHERE team_id = ?1", params![tid]).map_err(|e| format!("db: {e}"))?;
            conn.execute("DELETE FROM tenant_owners WHERE team_id = ?1", params![tid]).map_err(|e| format!("db: {e}"))?;
            let n = conn.execute("DELETE FROM teams WHERE id = ?1", params![tid]).map_err(|e| format!("db: {e}"))?;
            Ok(json!({ "ok": true, "deleted": n, "team_id": tid }))
        }
    }
}

fn run_tenant(conn: &mut Connection, cmd: &CloudTenantCmd) -> Result<Value, String> {
    match cmd {
        CloudTenantCmd::List => Ok(json!({ "ok": true, "tenants": crate::provision::tenant_list(conn)? })),
        CloudTenantCmd::Show { team } => {
            let tid = resolve_team_id(conn, team)?;
            let t = crate::provision::tenant_get(conn, &tid)?.ok_or_else(|| format!("tenant for team {tid} not found"))?;
            Ok(json!({ "ok": true, "tenant": t }))
        }
        CloudTenantCmd::Create { team, owner_user, pvc_size, server_image, kubeconfig, context } => {
            let tid = resolve_team_id(conn, team)?;
            if let Some(ps) = pvc_size {
                std::env::set_var("TEAMX_TENANT_PVC_SIZE", ps);
            }
            let kc = kubeconfig.as_ref().map(|p| p.to_string_lossy().to_string());
            let out = crate::provision::create_tenant(conn, &tid, server_image.as_deref(), kc.as_deref(), context.as_deref())?;
            if let Some(ou) = owner_user {
                crate::platform::tag_tenant_owner(conn, &tid, ou)?;
            }
            Ok(out)
        }
        CloudTenantCmd::Destroy { team, yes, kubeconfig, context } => {
            let tid = resolve_team_id(conn, team)?;
            if !*yes {
                return Err(format!("refusing to destroy tenant {tid} without --yes"));
            }
            let kc = kubeconfig.as_ref().map(|p| p.to_string_lossy().to_string());
            crate::provision::destroy_tenant(conn, &tid, kc.as_deref(), context.as_deref())
        }
        CloudTenantCmd::Endpoint { team, set } => {
            let tid = resolve_team_id(conn, team)?;
            crate::provision::set_tenant_endpoint(conn, &tid, set)
        }
        CloudTenantCmd::Ca(c) => run_ca(conn, c),
        CloudTenantCmd::Member(m) => run_member(conn, m),
        CloudTenantCmd::Start { .. } | CloudTenantCmd::Stop { .. } | CloudTenantCmd::Status { .. } => {
            Err("cloud tenant start/stop/status: not implemented yet (design/023 P3 — needs k8s scale/pod helpers)".to_string())
        }
    }
}

fn run_ca(conn: &mut Connection, cmd: &CloudCaCmd) -> Result<Value, String> {
    match cmd {
        CloudCaCmd::Audit { team, server_url, kubeconfig, context } => {
            let tid = resolve_team_id(conn, team)?;
            let kc = kubeconfig.as_ref().map(|p| p.to_string_lossy().to_string());
            crate::provision::tenant_ca_audit(conn, &tid, server_url.as_deref(), kc.as_deref(), context.as_deref())
        }
        CloudCaCmd::Show { .. } | CloudCaCmd::Import { .. } => {
            Err("cloud tenant ca show/import: not implemented yet (design/023 P3 — needs Secret swap + re-sign)".to_string())
        }
    }
}

fn run_member(conn: &mut Connection, cmd: &CloudMemberCmd) -> Result<Value, String> {
    match cmd {
        CloudMemberCmd::List { team, kubeconfig, context } => {
            let tid = resolve_team_id(conn, team)?;
            let kc = kubeconfig.as_ref().map(|p| p.to_string_lossy().to_string());
            crate::provision::tenant_service_post_via_endpoint(conn, &tid, kc.as_deref(), context.as_deref(), "team.roster", json!({ "team": tid }))
        }
        CloudMemberCmd::Add { team, name, role, user, auto_approve, save, kubeconfig, context } => {
            let tid = resolve_team_id(conn, team)?;
            let kc = kubeconfig.as_ref().map(|p| p.to_string_lossy().to_string());
            let out = crate::provision::tenant_service_post_via_endpoint(
                conn, &tid, kc.as_deref(), context.as_deref(), "team.invite",
                json!({
                    "team": tid, "role_desc": role.clone().unwrap_or_else(|| format!("{name}: member")),
                    "name_hint": name, "user": user, "auto_approve": auto_approve,
                }),
            )?;
            if let Some(p) = save {
                if let Some(l) = out.get("letter").and_then(Value::as_str) {
                    std::fs::write(p, l).map_err(|e| format!("write {}: {e}", p.display()))?;
                }
            }
            Ok(out)
        }
        CloudMemberCmd::Letter { team, member, server_url, save, kubeconfig, context } => {
            let tid = resolve_team_id(conn, team)?;
            let kc = kubeconfig.as_ref().map(|p| p.to_string_lossy().to_string());
            let out = crate::provision::tenant_service_post_via_endpoint(
                conn, &tid, kc.as_deref(), context.as_deref(), "team.reissue-letter",
                json!({ "team": tid, "member": member, "server_url": server_url }),
            )?;
            if let Some(p) = save {
                if let Some(l) = out.get("letter").and_then(Value::as_str) {
                    std::fs::write(p, l).map_err(|e| format!("write {}: {e}", p.display()))?;
                }
            }
            Ok(out)
        }
        CloudMemberCmd::Provision { team, member, all, workspace_gib, kubeconfig, context, session } => {
            let tid = resolve_team_id(conn, team)?;
            let kc = kubeconfig.as_ref().map(|p| p.to_string_lossy().to_string());
            let cx = context.as_deref();
            let _ = crate::commands::tcoin_actor(conn, session).map_err(|e| e.0)?;
            if *all {
                let seats = crate::provision::team_seat_ids(conn, &tid)?;
                if seats.is_empty() {
                    return Err(format!("team {tid} has no seats to provision"));
                }
                let mut items = Vec::new();
                for mid in &seats {
                    let r = crate::provision::provision_cloud_member(conn, &tid, mid, kc.as_deref(), cx, *workspace_gib, session)?;
                    items.push(json!({ "member": mid, "ok": r["ok"] }));
                }
                Ok(json!({ "ok": true, "provisioned": items.len(), "items": items }))
            } else {
                let mid = member.clone().ok_or_else(|| "member provision requires --member <id> or --all".to_string())?;
                let r = crate::provision::provision_cloud_member(conn, &tid, &mid, kc.as_deref(), cx, *workspace_gib, session)?;
                Ok(json!({ "ok": true, "item": { "member": mid, "result": r } }))
            }
        }
    }
}

fn run_join(conn: &mut Connection, cmd: &CloudJoinCmd) -> Result<Value, String> {
    match cmd {
        CloudJoinCmd::List { team, status } => {
            let tid = resolve_team_id(conn, team)?;
            let mut sql = String::from("SELECT id,user_id,status,member_id,decided_by FROM team_join_requests WHERE team_id = ?1");
            if status.is_some() {
                sql.push_str(" AND status = ?2");
            }
            sql.push_str(" ORDER BY created_at");
            let mut stmt = conn.prepare(&sql).map_err(|e| format!("db: {e}"))?;
            let rows: Vec<Value> = if let Some(st) = status {
                stmt.query_map(params![tid, st], |r| Ok(json!({ "id": r.get::<_, String>(0)?, "user_id": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?, "member_id": r.get::<_, Option<String>>(3)?, "decided_by": r.get::<_, Option<String>>(4)? })))
                    .map_err(|e| format!("db: {e}"))?.collect::<rusqlite::Result<_>>().map_err(|e| format!("db: {e}"))?
            } else {
                stmt.query_map(params![tid], |r| Ok(json!({ "id": r.get::<_, String>(0)?, "user_id": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?, "member_id": r.get::<_, Option<String>>(3)?, "decided_by": r.get::<_, Option<String>>(4)? })))
                    .map_err(|e| format!("db: {e}"))?.collect::<rusqlite::Result<_>>().map_err(|e| format!("db: {e}"))?
            };
            Ok(json!({ "ok": true, "requests": rows }))
        }
        CloudJoinCmd::Import { team, user, member, status, decided_by } => {
            let tid = resolve_team_id(conn, team)?;
            let (uid, _) = resolve_user(conn, user)?;
            let id = uuid::Uuid::new_v4().to_string();
            let now = crate::db::now();
            conn.execute(
                "INSERT INTO team_join_requests (id,team_id,user_id,status,message,created_at,decided_at,decided_by,cooldown_until,member_id,letter,pod_state,pod_detail)
                 VALUES (?1,?2,?3,?4,'migrated',?5,?5,?6,'',?7,'','','')",
                params![id, tid, uid, status, now, decided_by, member],
            )
            .map_err(|e| format!("db: {e}"))?;
            Ok(json!({ "ok": true, "id": id, "team_id": tid, "user_id": uid, "member_id": member, "status": status }))
        }
        CloudJoinCmd::Approve { .. } | CloudJoinCmd::Deny { .. } => {
            Err("cloud join approve/deny: not implemented yet (use `teamx discovery` / the platform console)".to_string())
        }
    }
}

fn run_bootstrap(conn: &mut Connection, cmd: &CloudBootstrapCmd) -> Result<Value, String> {
    match cmd {
        CloudBootstrapCmd::Token { ttl } => crate::platform::issue_bootstrap_token(conn, *ttl),
        CloudBootstrapCmd::Redeem { token, user, name } => crate::platform::redeem_bootstrap_token(conn, token, user, name.as_deref().unwrap_or("")),
        CloudBootstrapCmd::Admins => Ok(json!({ "ok": true, "admins": crate::platform::list_admins(conn)? })),
        CloudBootstrapCmd::AdminRevoke { user } => crate::platform::revoke_admin(conn, user),
    }
}
