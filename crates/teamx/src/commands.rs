use crate::cli::{CertCmd, Cli, Command, GoalCmd, LoopxCmd, MemberCmd, ProxyCmd, RoleCmd, TeamCmd, TunnelCmd};
use crate::db::{self, DEFAULT_ROLES};
use crate::events;
use crate::loopx;
use crate::pki;
use crate::state::{Action, GoalState, MemberState, TeamState};
use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

pub struct AppError(pub String);

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::fmt::Debug for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for AppError {}

type Result<T> = std::result::Result<T, AppError>;

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(AppError(msg.into()))
}

// ---------------------------------------------------------------------------
// Row structs
// ---------------------------------------------------------------------------

#[derive(Clone)]
#[allow(dead_code)]
struct TeamRow {
    id: String,
    name: String,
    owner_member_id: Option<String>,
    goal_id: Option<String>,
    state: String,
    invite_token: String,
    created_at: String,
}

#[derive(Clone)]
#[allow(dead_code)]
struct MemberRow {
    id: String,
    team_id: String,
    session_key: String,
    display_name: String,
    role: Option<String>,
    state: String,
    loopx_project: Option<String>,
    joined_at: String,
    left_at: Option<String>,
}

#[derive(Clone)]
#[allow(dead_code)]
struct GoalRow {
    id: String,
    team_id: String,
    title: String,
    body: Option<String>,
    state: String,
}

#[derive(Clone)]
struct QuestionRow {
    id: String,
    team_id: String,
    asker_member_id: String,
    target_member_id: String,
    question: String,
    answer: Option<String>,
    state: String,
    created_at: String,
}

#[derive(Clone)]
#[allow(dead_code)]
struct InvitationRow {
    id: String,
    team_id: String,
    member_id: String,
    role_key: String,
    role_label: Option<String>,
    role_desc: Option<String>,
    cert_serial: Option<String>,
    cert_cn: Option<String>,
    created_by: String,
    created_at: String,
    used_by: Option<String>,
    used_at: Option<String>,
    revoked_at: Option<String>,
}

fn team_row(r: &rusqlite::Row) -> rusqlite::Result<TeamRow> {
    Ok(TeamRow {
        id: r.get(0)?,
        name: r.get(1)?,
        owner_member_id: r.get(2)?,
        goal_id: r.get(3)?,
        state: r.get(4)?,
        invite_token: r.get(5)?,
        created_at: r.get(6)?,
    })
}

fn member_row(r: &rusqlite::Row) -> rusqlite::Result<MemberRow> {
    Ok(MemberRow {
        id: r.get(0)?,
        team_id: r.get(1)?,
        session_key: r.get(2)?,
        display_name: r.get(3)?,
        role: r.get(4)?,
        state: r.get(5)?,
        loopx_project: r.get(6)?,
        joined_at: r.get(7)?,
        left_at: r.get(8)?,
    })
}

fn goal_row(r: &rusqlite::Row) -> rusqlite::Result<GoalRow> {
    Ok(GoalRow {
        id: r.get(0)?,
        team_id: r.get(1)?,
        title: r.get(2)?,
        body: r.get(3)?,
        state: r.get(4)?,
    })
}

fn question_row(r: &rusqlite::Row) -> rusqlite::Result<QuestionRow> {
    Ok(QuestionRow {
        id: r.get(0)?,
        team_id: r.get(1)?,
        asker_member_id: r.get(2)?,
        target_member_id: r.get(3)?,
        question: r.get(4)?,
        answer: r.get(5)?,
        state: r.get(6)?,
        created_at: r.get(7)?,
    })
}

fn invitation_row(r: &rusqlite::Row) -> rusqlite::Result<InvitationRow> {
    Ok(InvitationRow {
        id: r.get(0)?,
        team_id: r.get(1)?,
        member_id: r.get(2)?,
        role_key: r.get(3)?,
        role_label: r.get(4)?,
        role_desc: r.get(5)?,
        cert_serial: r.get(6)?,
        cert_cn: r.get(7)?,
        created_by: r.get(8)?,
        created_at: r.get(9)?,
        used_by: r.get(10)?,
        used_at: r.get(11)?,
        revoked_at: r.get(12)?,
    })
}

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

fn team_by_id(conn: &Connection, team_id: &str) -> Result<TeamRow> {
    conn.query_row(
        "SELECT id, name, owner_member_id, goal_id, state, invite_token, created_at
         FROM teams WHERE id = ?1",
        [team_id],
        team_row,
    )
    .map_err(|e| AppError(format!("team {team_id} not found: {e}")))
}

fn team_by_token(conn: &Connection, token: &str) -> Result<TeamRow> {
    conn.query_row(
        "SELECT id, name, owner_member_id, goal_id, state, invite_token, created_at
         FROM teams WHERE invite_token = ?1",
        [token],
        team_row,
    )
    .map_err(|_| AppError("invalid invite token: no team matches".into()))
}

fn goal_by_id(conn: &Connection, goal_id: &str) -> Result<GoalRow> {
    conn.query_row(
        "SELECT id, team_id, title, body, state FROM goals WHERE id = ?1",
        [goal_id],
        goal_row,
    )
    .map_err(|e| AppError(format!("goal {goal_id} not found: {e}")))
}

fn team_goal(conn: &Connection, team_id: &str) -> Result<Option<GoalRow>> {
    let team = team_by_id(conn, team_id)?;
    match team.goal_id {
        Some(gid) => Ok(Some(goal_by_id(conn, &gid)?)),
        None => Ok(None),
    }
}

fn members_for_team(conn: &Connection, team_id: &str) -> rusqlite::Result<Vec<MemberRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, team_id, session_key, display_name, role, state, loopx_project, joined_at, left_at
         FROM members WHERE team_id = ?1 ORDER BY joined_at ASC",
    )?;
    let rows = stmt.query_map([team_id], member_row)?;
    rows.collect()
}

fn member_by_id(conn: &Connection, member_id: &str) -> Result<MemberRow> {
    conn.query_row(
        "SELECT id, team_id, session_key, display_name, role, state, loopx_project, joined_at, left_at
         FROM members WHERE id = ?1",
        [member_id],
        member_row,
    )
    .map_err(|e| AppError(format!("member {member_id} not found: {e}")))
}

/// Active memberships (state != left/denied) for a session, excluding
/// memberships in soft-destroyed teams (they are hidden from lists/status).
fn memberships_for_session(
    conn: &Connection,
    session_key: &str,
) -> rusqlite::Result<Vec<MemberRow>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.team_id, m.session_key, m.display_name, m.role, m.state, m.loopx_project, m.joined_at, m.left_at
         FROM members m
         JOIN teams t ON t.id = m.team_id
         WHERE m.session_key = ?1 AND m.state NOT IN ('left','denied') AND t.state != 'destroyed'
         ORDER BY m.joined_at ASC",
    )?;
    let rows = stmt.query_map([session_key], member_row)?;
    rows.collect()
}

/// Resolve the member + team for an actor session.
/// `team_opt` disambiguates when the session belongs to several teams.
fn resolve_actor(conn: &Connection, session: &str, team_opt: Option<&str>) -> Result<(MemberRow, TeamRow)> {
    let members = memberships_for_session(conn, session)
        .map_err(|e| AppError(format!("db error: {e}")))?;
    if members.is_empty() {
        return err(format!(
            "session `{session}` is not a member of any team. Join one first (teamx team join <token> ...)."
        ));
    }
    let m = match team_opt {
        Some(tid) => members
            .into_iter()
            .find(|m| m.team_id == tid)
            .ok_or_else(|| {
                AppError(format!("session `{session}` is not a member of team {tid}"))
            })?,
        None => {
            if members.len() == 1 {
                members.into_iter().next().unwrap()
            } else {
                let list: Vec<String> = members.iter().map(|m| m.team_id.clone()).collect();
                return err(format!(
                    "session `{session}` belongs to multiple teams; pass --team (one of {list:?})"
                ));
            }
        }
    };
    let team = team_by_id(conn, &m.team_id)?;
    Ok((m, team))
}

fn ensure_owner(_conn: &Connection, actor: &MemberRow, team: &TeamRow) -> Result<()> {
    if team.owner_member_id.as_deref() == Some(actor.id.as_str()) {
        Ok(())
    } else {
        err(format!(
            "only the team lead may do this (owner member {})",
            team.owner_member_id.as_deref().unwrap_or("<none>")
        ))
    }
}

fn touch(conn: &Connection, member_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE members SET last_seen_at = ?1 WHERE id = ?2",
        params![db::now(), member_id],
    )?;
    Ok(())
}

/// Seeded role catalog for a team.
fn seed_roles(tx: &Transaction, team_id: &str) -> rusqlite::Result<()> {
    for (key, label, desc) in DEFAULT_ROLES {
        tx.execute(
            "INSERT OR IGNORE INTO roles (team_id, key, label, description, permissions_json)
             VALUES (?1, ?2, ?3, ?4, '{}')",
            params![team_id, key, label, desc],
        )?;
    }
    Ok(())
}

/// True if a role row exists in this team (any state).
fn role_exists(conn: &Connection, team_id: &str, key: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM roles WHERE team_id = ?1 AND key = ?2",
        params![team_id, key],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

/// True if the role exists AND is approved (usable via `role set`).
fn role_approved(conn: &Connection, team_id: &str, key: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM roles WHERE team_id = ?1 AND key = ?2 AND state = 'approved'",
        params![team_id, key],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

/// Role state (proposed/approved) for a role, if any.
fn role_state(conn: &Connection, team_id: &str, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT state FROM roles WHERE team_id = ?1 AND key = ?2",
        params![team_id, key],
        |r| r.get(0),
    )
    .optional()
}

fn role_label(conn: &Connection, team_id: &str, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT label FROM roles WHERE team_id = ?1 AND key = ?2",
        params![team_id, key],
        |r| r.get(0),
    )
    .optional()
}

fn invitation_by_id(conn: &Connection, id: &str) -> Result<InvitationRow> {
    conn.query_row(
        "SELECT id, team_id, member_id, role_key, role_label, role_desc, cert_serial, cert_cn,
                created_by, created_at, used_by, used_at, revoked_at
         FROM invitations WHERE id = ?1",
        [id],
        invitation_row,
    )
    .map_err(|_| AppError(format!("invitation {id} not found")))
}

fn invitation_by_id_opt(conn: &Connection, id: &str) -> rusqlite::Result<Option<InvitationRow>> {
    conn.query_row(
        "SELECT id, team_id, member_id, role_key, role_label, role_desc, cert_serial, cert_cn,
                created_by, created_at, used_by, used_at, revoked_at
         FROM invitations WHERE id = ?1",
        [id],
        invitation_row,
    )
    .optional()
}

/// Network mode: resolve a member's session key by id. The actor identity comes
/// from the client certificate CN (`member:<id>:<role>`), not a self-reported
/// session, so this is the bridge back into the existing session-based commands.
pub fn session_key_for_member(conn: &Connection, member_id: &str) -> Result<String> {
    conn.query_row(
        "SELECT session_key FROM members WHERE id = ?1",
        [member_id],
        |r| r.get(0),
    )
    .map_err(|_| AppError(format!("member {member_id} not found (has it joined/imported yet?)")))
}

// ---------------------------------------------------------------------------
// Event helpers (inside write transaction)
// ---------------------------------------------------------------------------

fn emit_json(
    tx: &mut Transaction,
    team_id: &str,
    member_id: Option<&str>,
    event_type: &str,
    payload: Value,
) -> rusqlite::Result<i64> {
    events::emit(tx, team_id, member_id, event_type, Some(&payload))
}

// ---------------------------------------------------------------------------
// Public: execute
// ---------------------------------------------------------------------------

pub fn execute(cli: &Cli, conn: &mut Connection) -> Result<Value> {
    let out = match &cli.command {
        Command::Init => cmd_init(conn)?,
        Command::Team(t) => match t {
            TeamCmd::Create { name, session, goal_title, goal_body } => {
                cmd_team_create(conn, name, session, goal_title.as_deref(), goal_body.as_deref())?
            }
            TeamCmd::Join { token, name, session, loopx_project } => {
                cmd_team_join(conn, token, name, session, loopx_project.as_deref().and_then(|p| p.to_str()))?
            }
            TeamCmd::Approve { member_id, session, team } => cmd_team_decide(conn, member_id, session, team.as_deref(), true)?,
            TeamCmd::Deny { member_id, session, team } => cmd_team_decide(conn, member_id, session, team.as_deref(), false)?,
            TeamCmd::List { session } => cmd_team_list(conn, session)?,
            TeamCmd::Status { team, session } => cmd_team_status(conn, team.as_deref(), session.as_deref())?,
            TeamCmd::Leave { session, team } => cmd_team_leave(conn, session, team.as_deref())?,
            TeamCmd::Archive { session, team } => cmd_team_archive(conn, session, team.as_deref())?,
            TeamCmd::Destroy { session, team } => cmd_team_destroy(conn, session, team.as_deref())?,
            TeamCmd::Invite { role_desc, name_hint, server_url, session, team } => {
                cmd_team_invite(conn, role_desc, name_hint.as_deref(), server_url.as_deref(), session, team.as_deref())?
            }
            TeamCmd::InviteList { session, team } => cmd_team_invite_list(conn, session, team.as_deref())?,
            TeamCmd::InviteRevoke { id, session, team } => cmd_team_invite_revoke(conn, id, session, team.as_deref())?,
            TeamCmd::Import { letter, name, session } => cmd_team_import(conn, letter, name.as_deref(), session, None)?,
        },
        Command::Goal(g) => match g {
            GoalCmd::Set { title, body, session, team } => {
                cmd_goal_set(conn, title, body.as_deref(), session, team.as_deref())?
            }
            GoalCmd::Share { session, team } => cmd_goal_share(conn, session, team.as_deref())?,
            GoalCmd::Close { session, team } => cmd_goal_close(conn, session, team.as_deref())?,
        },
        Command::Member(m) => match m {
            MemberCmd::SetState { state, member, session, team } => {
                cmd_member_set_state(conn, state, member.as_deref(), session, team.as_deref())?
            }
        },
        Command::Role(r) => match r {
            RoleCmd::List { team } => cmd_role_list(conn, team.as_deref())?,
            RoleCmd::Set { role, session, member, team } => {
                cmd_role_set(conn, role, session, member.as_deref(), team.as_deref())?
            }
            RoleCmd::Propose { role, label, description, session, team } => {
                cmd_role_propose(conn, role, label, description.as_deref(), session, team.as_deref())?
            }
            RoleCmd::Approve { role, session, team } => {
                cmd_role_approve(conn, role, session, team.as_deref())?
            }
            RoleCmd::Deny { role, session, team } => {
                cmd_role_deny(conn, role, session, team.as_deref())?
            }
            RoleCmd::Update { role, label, description, session, team } => {
                cmd_role_update(conn, role, label.as_deref(), description.as_deref(), session, team.as_deref())?
            }
        },
        Command::Publish { r#type, data, assignee, session, team } => {
            cmd_publish(conn, r#type, data.as_deref(), assignee.as_deref(), session, team.as_deref())?
        }
        Command::Ask { member_id, question, session, team } => {
            cmd_ask(conn, member_id, question, session, team.as_deref())?
        }
        Command::Respond { ask_id, answer, session } => {
            cmd_respond(conn, ask_id, answer, session)?
        }
        Command::Events { after, team } => cmd_events(conn, *after, team.as_deref())?,
        Command::Log { team, session, limit, after } => {
            cmd_log(conn, team.as_deref(), session.as_deref(), *limit, *after)?
        }
        Command::Sync { session, no_advance } => cmd_sync(conn, session, *no_advance)?,
        Command::Loopx(l) => match l {
            LoopxCmd::Report { project, session, team } => {
                cmd_loopx_report(conn, project, session, team.as_deref())?
            }
        },
        // `teamx serve` never reaches here (handled in main); this arm exists
        // only so the match stays exhaustive.
        Command::Serve(_) => {
            return err("`teamx serve` must be run as its own process")
        }
        Command::Cert(c) => match c {
            CertCmd::Init => cmd_cert_init()?,
            CertCmd::Issue { member_id, role, out } => cmd_cert_issue(member_id, role, out.as_deref())?,
            CertCmd::Ca => cmd_cert_ca()?,
        },
        // Tunnel commands: network-mode only. `expose`/`forward` are long-lived
        // WS clients; `list`/`status`/`close` are HTTP RPC calls.
        Command::Tunnel(cmd) => {
            let url = resolve_server_url(match cmd {
                TunnelCmd::Expose { server, .. } => server.as_deref(),
                TunnelCmd::List { server, .. } => server.as_deref(),
                TunnelCmd::Status { server, .. } => server.as_deref(),
                TunnelCmd::Close { server, .. } => server.as_deref(),
                TunnelCmd::Forward { server, .. } => server.as_deref(),
            })?;
            let result = match cmd {
                TunnelCmd::Expose { name, port, lan_ip, mode, .. } => {
                    crate::tunnel_client::expose(&url, name, *port, mode, lan_ip.as_deref())
                }
                TunnelCmd::List { .. } => crate::tunnel_client::rpc(&url, "tunnel.list", serde_json::json!({})),
                TunnelCmd::Status { name, .. } => {
                    crate::tunnel_client::rpc(&url, "tunnel.status", serde_json::json!({ "name": name }))
                }
                TunnelCmd::Close { name, .. } => {
                    crate::tunnel_client::rpc(&url, "tunnel.close", serde_json::json!({ "name": name }))
                }
                TunnelCmd::Forward { name, local_port, .. } => {
                    crate::tunnel_client::forward(&url, name, local_port.unwrap_or(0))
                }
            };
            return result.map_err(AppError);
        }
        // Proxy commands: network-mode only, long-lived WS clients.
        Command::Proxy(cmd) => {
            let url = resolve_server_url(match cmd {
                ProxyCmd::Exit { server, .. } => server.as_deref(),
                ProxyCmd::Start { server, .. } => server.as_deref(),
            })?;
            let result = match cmd {
                ProxyCmd::Exit { name, .. } => crate::tunnel_client::proxy_exit(&url, name),
                ProxyCmd::Start { port, exit, .. } => crate::tunnel_client::socks5_proxy(&url, exit, *port),
            };
            return result.map_err(AppError);
        }
    };
    Ok(out)
}

/// Resolve the network-mode server URL: `--server` flag > `TEAMX_SERVER_URL`
/// env > auto-discovered from an imported letter > default localhost.
fn resolve_server_url(explicit: Option<&str>) -> Result<String> {
    if let Some(u) = explicit {
        return Ok(u.to_string());
    }
    if let Ok(u) = std::env::var("TEAMX_SERVER_URL") {
        if !u.is_empty() {
            return Ok(u);
        }
    }
    // Fall back to the letter's embedded server URL.
    if let Some(u) = crate::tunnel_client::discover_server_url() {
        return Ok(u);
    }
    Ok("https://127.0.0.1:5781".to_string())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn cmd_init(conn: &mut Connection) -> Result<Value> {
    db::migrate(conn).map_err(|e| AppError(format!("init failed: {e}")))?;
    Ok(json!({ "ok": true, "schema": "teamx-v1" }))
}

fn cmd_team_create(
    conn: &mut Connection,
    name: &str,
    session: &str,
    goal_title: Option<&str>,
    goal_body: Option<&str>,
) -> Result<Value> {
    // Reject empty team names (model-driven calls can pass "" accidentally).
    if name.trim().is_empty() {
        return err("team name cannot be empty");
    }
    // Idempotency guard for model-driven retries: if this session already owns
    // a non-archived team with the same name, return it instead of duplicating.
    let existing: Option<(String, String, String)> = conn
        .query_row(
            "SELECT t.id, t.state, t.invite_token
             FROM teams t JOIN members m ON m.id = t.owner_member_id
             WHERE m.session_key = ?1 AND t.name = ?2 AND t.state NOT IN ('archived')",
            params![session, name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| AppError(format!("db error: {e}")))?;
    if let Some((team_id, state, token)) = existing {
        return Ok(json!({
            "ok": true,
            "reused": true,
            "note": "a team with this name already exists for this session; reusing it",
            "team": { "id": team_id, "name": name, "state": state, "invite_token": token },
        }));
    }

    // A session may own at most one non-archived team. Being a member of other
    // teams is fine (role != owner); only the "create" path is restricted so a
    // session cannot become the owner of several teams at once.
    let owned: Option<(String, String)> = conn
        .query_row(
            "SELECT t.id, t.name FROM teams t
             JOIN members m ON m.id = t.owner_member_id
             WHERE m.session_key = ?1 AND t.state NOT IN ('archived')
             ORDER BY t.created_at LIMIT 1",
            params![session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| AppError(format!("db error: {e}")))?;
    if let Some((team_id, team_name)) = owned {
        return err(format!(
            "session `{session}` already owns team `{team_name}` ({team_id}); one session can only own one team (archive it first, or join other teams as a member)"
        ));
    }

    let team_id = uuid::Uuid::new_v4().to_string();
    let member_id = uuid::Uuid::new_v4().to_string();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let now = db::now();

    db::with_write(conn, |tx| {
        tx.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES (?1, ?2, ?3, NULL, 'forming', ?4, ?5, ?5)",
            params![team_id, name, member_id, token, now],
        )?;
        tx.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES (?1, ?2, ?3, ?4, 'owner', 'active', ?5)",
            params![member_id, team_id, session, name, now],
        )?;
        seed_roles(tx, &team_id)?;
        emit_json(tx, &team_id, Some(&member_id), "team.created", json!({ "name": name }))?;
        Ok(())
    })
    .map_err(|e| AppError(format!("team create failed: {e}")))?;

    // optional initial goal
    let mut goal_id = None;
    if let Some(title) = goal_title {
        goal_id = Some(cmd_goal_set_inner(conn, &team_id, &member_id, title, goal_body)?);
    }

    // TEAM.md bootstrap: if `.teamx/TEAM.md` exists in the project root, parse it
    // and auto-initialize — set the goal (if none set yet), issue per-member
    // invitation letters, generate member AGENTS.md and create work directories.
    let mut teamfile_info = None;
    if let Ok(cwd) = std::env::current_dir() {
        match crate::teamfile::load_team_file(&cwd) {
            Ok(Some(tf)) => {
                let boot = bootstrap_from_teamfile(conn, &team_id, &member_id, session, &tf, goal_id.is_none())?;
                if goal_id.is_none() {
                    goal_id = boot.goal_id;
                }
                teamfile_info = Some(json!({
                    "file": cwd.join(".teamx").join("TEAM.md").display().to_string(),
                    "team_name": tf.team_name,
                    "goals": tf.goals,
                    "members": boot.members,
                    "note": "TEAM.md detected; team bootstrapped",
                }));
            }
            Ok(None) => {}
            Err(e) => {
                // Invalid TEAM.md should not block team creation: surface a warning.
                teamfile_info = Some(json!({ "error": e, "note": "TEAM.md present but could not be parsed; team created without bootstrap" }));
            }
        }
    }

    let team = team_by_id(conn, &team_id)?;
    let mut out = json!({
        "ok": true,
        "team": { "id": team.id, "name": team.name, "state": team.state, "invite_token": team.invite_token },
        "owner_member_id": member_id,
        "goal_id": goal_id,
    });
    if let Some(info) = teamfile_info {
        if let Some(o) = out.as_object_mut() {
            o.insert("teamfile".to_string(), info);
        }
    }
    Ok(out)
}

/// Result of a TEAM.md bootstrap run.
struct BootstrapOutcome {
    goal_id: Option<String>,
    members: Vec<serde_json::Value>,
}

/// Parse TEAM.md and auto-initialize the team: goal (if none set), per-member
/// invitation letters (saved + printed), member AGENTS.md, member work dirs.
fn bootstrap_from_teamfile(
    conn: &mut Connection,
    team_id: &str,
    owner_member_id: &str,
    owner_session: &str,
    tf: &crate::teamfile::TeamFile,
    set_goal: bool,
) -> Result<BootstrapOutcome> {
    let cwd = std::env::current_dir().map_err(|e| AppError(format!("cwd: {e}")))?;
    let teamx_dir = cwd.join(".teamx");
    let members_dir = teamx_dir.join("members");
    let server_url = std::env::var("TEAMX_SERVER_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://127.0.0.1:5781".to_string());

    // 1. Goal: title = team name (or first goal), body = background + goals.
    let mut goal_id = None;
    if set_goal {
        let title = if !tf.team_name.is_empty() { tf.team_name.as_str() } else { "team goal" };
        let mut body = Vec::new();
        if let Some(b) = &tf.background {
            body.push(b.clone());
        }
        for g in &tf.goals {
            body.push(format!("- {g}"));
        }
        let body = if body.is_empty() { None } else { Some(body.join("\n")) };
        goal_id = Some(cmd_goal_set_inner(conn, team_id, owner_member_id, title, body.as_deref())?);
    }

    // Project-root AGENTS.md (if present) to merge into each member's AGENTS.md.
    let root_agents = cwd.join("AGENTS.md");
    let root_agents_text = std::fs::read_to_string(&root_agents).ok();

    // 2. Per-member bootstrap: letter + AGENTS.md + work dir.
    let mut members = Vec::new();
    for (i, m) in tf.members.iter().enumerate() {
        // Skip the owner member (the creating session already owns the team).
        let is_owner = m.role.as_deref() == Some("owner") && i == 0;
        let role_desc = m
            .description
            .as_ref()
            .map(|d| format!("{}: {}", m.role.as_deref().unwrap_or("contributor"), d))
            .unwrap_or_else(|| m.role.clone().unwrap_or_else(|| "contributor".to_string()));

        // 2a. Invitation letter (role from TEAM.md, member name as hint).
        let mut letter_value = None;
        let mut letter_file = None;
        if !is_owner {
            let inv = cmd_team_invite(conn, &role_desc, Some(&m.display_name), Some(&server_url), owner_session, Some(team_id))?;
            let letter = inv.get("letter").and_then(|l| l.as_str()).unwrap_or("").to_string();
            letter_value = Some(inv.clone());

            // Save the letter into the member's work dir.
            let mdir = members_dir.join(&m.key);
            std::fs::create_dir_all(&mdir).map_err(|e| AppError(format!("mkdir {mdir:?}: {e}")))?;
            let lp = mdir.join("invitation.letter");
            // The letter embeds the member's private key — keep it 0600.
            write_private(&lp, &letter)?;
            letter_file = Some(lp.display().to_string());
        }

        // 2b. Member AGENTS.md (project root AGENTS.md + member profile).
        let mdir = members_dir.join(&m.key);
        std::fs::create_dir_all(&mdir).map_err(|e| AppError(format!("mkdir {mdir:?}: {e}")))?;
        let agents = build_member_agents(root_agents_text.as_deref(), tf, m);
        let ap = mdir.join("AGENTS.md");
        std::fs::write(&ap, &agents).map_err(|e| AppError(format!("write AGENTS.md {ap:?}: {e}")))?;

        members.push(json!({
            "name": m.display_name,
            "key": m.key,
            "role": m.role,
            "letter": letter_value,
            "letter_file": letter_file,
            "agents_file": ap.display().to_string(),
            "workdir": mdir.display().to_string(),
        }));
    }

    Ok(BootstrapOutcome { goal_id, members })
}

/// Build a member-specific AGENTS.md by merging the project-root AGENTS.md
/// (if any) with the member's profile from TEAM.md.
fn build_member_agents(root_agents: Option<&str>, tf: &crate::teamfile::TeamFile, m: &crate::teamfile::MemberProfile) -> String {
    let mut s = String::new();
    s.push_str(&format!("# AGENTS.md — {}（{}）\n\n", m.display_name, m.role.clone().unwrap_or_else(|| "member".to_string())));
    if let Some(root) = root_agents {
        if !root.trim().is_empty() {
            s.push_str("## 来自项目根 AGENTS.md\n\n");
            s.push_str(root.trim());
            s.push_str("\n\n");
        }
    }
    s.push_str("## 团队角色\n\n");
    s.push_str(&format!("- 角色: {}\n", m.role.clone().unwrap_or_else(|| "member".to_string())));
    if let Some(d) = &m.description {
        s.push_str(&format!("- 分工: {d}\n"));
    }
    if !m.skills.is_empty() {
        s.push_str(&format!("- 技能: {}\n", m.skills.join(", ")));
    }
    if !m.outputs.is_empty() {
        s.push_str(&format!("- 工作输出: {}\n", m.outputs.join(", ")));
    }
    s.push_str("\n## 团队上下文\n\n");
    s.push_str(&format!("- 团队: {}\n", if tf.team_name.is_empty() { "team" } else { &tf.team_name }));
    s.push_str(&format!("- 成员目录: `.teamx/members/{}/`\n", m.key));
    s.push_str("- 工作方式: 通过 `teamx` 工具同步进度、查阅团队事件、汇报结果。\n");
    s
}

fn cmd_team_join(
    conn: &mut Connection,
    token: &str,
    name: &str,
    session: &str,
    loopx_project: Option<&str>,
) -> Result<Value> {
    let team = team_by_token(conn, token)?;
    // `destroyed` teams are hidden from every listing; joining one would leave
    // the member pending forever with no way to see or leave the team.
    if matches!(team.state.as_str(), "completed" | "archived" | "destroyed") {
        return err(format!("team `{}` is {} and no longer accepts members", team.name, team.state));
    }

    // A session has exactly one member row per team (enforced by a unique index).
    // Rejoin after leave/deny reactivates that row instead of inserting a new one.
    let existing: Option<(String, String)> = conn
        .query_row(
            "SELECT id, state FROM members WHERE team_id = ?1 AND session_key = ?2",
            params![team.id, session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| AppError(format!("db error: {e}")))?;

    let now = db::now();
    let member_id = match existing {
        Some((id, state)) if state == "left" || state == "denied" => {
            db::with_write(conn, |tx| {
                tx.execute(
                    "UPDATE members SET state = 'pending', role = NULL, display_name = ?1,
                     loopx_project = ?2, joined_at = ?3, left_at = NULL WHERE id = ?4",
                    params![name, loopx_project, now, id],
                )?;
                emit_json(
                    tx,
                    &team.id,
                    Some(&id),
                    "membership.pending",
                    json!({ "display_name": name, "team": team.name, "loopx_project": loopx_project, "rejoined": true }),
                )?;
                Ok(())
            })
            .map_err(|e| AppError(format!("join failed: {e}")))?;
            id
        }
        Some((_, state)) => {
            return err(format!("session is already a member of this team (state {state})"));
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            db::with_write(conn, |tx| {
                tx.execute(
                    "INSERT INTO members (id, team_id, session_key, display_name, role, state, loopx_project, joined_at)
                     VALUES (?1, ?2, ?3, ?4, NULL, 'pending', ?5, ?6)",
                    params![id, team.id, session, name, loopx_project, now],
                )?;
                emit_json(
                    tx,
                    &team.id,
                    Some(&id),
                    "membership.pending",
                    json!({ "display_name": name, "team": team.name, "loopx_project": loopx_project }),
                )?;
                Ok(())
            })
            .map_err(|e| AppError(format!("join failed: {e}")))?;
            id
        }
    };

    Ok(json!({
        "ok": true,
        "status": "pending",
        "member_id": member_id,
        "team": { "id": team.id, "name": team.name, "state": team.state },
        "note": "waiting for owner approval",
    }))
}

fn cmd_team_decide(conn: &mut Connection, member_id: &str, session: &str, team_opt: Option<&str>, approve: bool) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    let target = member_by_id(conn, member_id)?;
    if target.team_id != team.id {
        return err(format!("member {member_id} is not in team {}", team.id));
    }
    if target.state != "pending" {
        return err(format!("member {member_id} is not pending (state: {})", target.state));
    }
    let from = MemberState::Pending;
    let action = if approve { Action::Approve } else { Action::Deny };
    let to = crate::state::member_transition(from, &action)
        .map_err(AppError)?;
    let from_s = from.as_str();
    let to_s = to.as_str();

    db::with_write(conn, |tx| {
        tx.execute("UPDATE members SET state = ?1 WHERE id = ?2", params![to_s, target.id])?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            if approve { "membership.approved" } else { "membership.denied" },
            json!({ "member_id": target.id, "display_name": target.display_name }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("decision failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "action": if approve { "approved" } else { "denied" }, "member_id": target.id, "state": to_s, "from": from_s }))
}

fn cmd_team_list(conn: &Connection, session: &str) -> Result<Value> {
    let members = memberships_for_session(conn, session).map_err(|e| AppError(format!("db error: {e}")))?;
    let mut teams = Vec::new();
    for m in members {
        let team = team_by_id(conn, &m.team_id)?;
        let goal = team_goal(conn, &team.id)?;
        teams.push(json!({
            "team_id": team.id,
            "name": team.name,
            "state": team.state,
            "my_role": m.role,
            "my_state": m.state,
            "goal": goal.map(|g| json!({ "title": g.title, "state": g.state })),
            "invite_token": team.invite_token,
        }));
    }
    Ok(json!({ "teams": teams }))
}

fn team_status_json(conn: &Connection, team: &TeamRow) -> Result<Value> {
    let goal = team_goal(conn, &team.id)?;
    let members = members_for_team(conn, &team.id).map_err(|e| AppError(format!("db error: {e}")))?;
    let questions = open_questions(conn, &team.id).map_err(|e| AppError(format!("db error: {e}")))?;
    let roles = roles_json(conn, &team.id).map_err(|e| AppError(format!("db error: {e}")))?;
    let recent = events::recent(conn, &team.id, 20)
        .map_err(|e| AppError(format!("db error: {e}")))?;
    let recent: Vec<Value> = recent.iter().map(event_json).collect();
    Ok(json!({
        "team": {
            "id": team.id,
            "name": team.name,
            "state": team.state,
            "invite_token": team.invite_token,
            "owner_member_id": team.owner_member_id,
        },
        "goal": goal.map(|g| json!({ "id": g.id, "title": g.title, "body": g.body, "state": g.state })),
        "members": members.iter().map(member_json).collect::<Vec<_>>(),
        "questions": questions.iter().map(question_json).collect::<Vec<_>>(),
        "roles": roles,
        "recent_events": recent,
    }))
}

fn cmd_team_status(conn: &Connection, team_opt: Option<&str>, session_opt: Option<&str>) -> Result<Value> {
    match team_opt {
        Some(tid) => {
            let team = team_by_id(conn, tid)?;
            Ok(json!({ "teams": [team_status_json(conn, &team)?] }))
        }
        None => match session_opt {
            Some(session) => {
                let members = memberships_for_session(conn, session)
                    .map_err(|e| AppError(format!("db error: {e}")))?;
                if members.is_empty() {
                    return err(format!(
                        "session `{session}` is not a member of any team. Join one first (teamx team join <token> ...)."
                    ));
                }
                if members.len() > 1 {
                    let list: Vec<String> = members.iter().map(|m| m.team_id.clone()).collect();
                    return err(format!(
                        "session `{session}` belongs to multiple teams; pass --team (one of {list:?})"
                    ));
                }
                let team = team_by_id(conn, &members[0].team_id)?;
                Ok(json!({ "teams": [team_status_json(conn, &team)?] }))
            }
            None => err("teamx team status requires --team <id> or --session <key>"),
        },
    }
}

fn cmd_team_leave(conn: &mut Connection, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    if team.owner_member_id.as_deref() == Some(actor.id.as_str()) {
        return err(
            "the team lead cannot leave (there is no ownership transfer yet). \
             Close the goal or leave the team as-is.",
        );
    }
    let from = MemberState::from_str(&actor.state).unwrap_or(MemberState::Active);
    let to = crate::state::member_transition(from, &Action::Leave).map_err(AppError)?;
    let from_s = from.as_str();
    let to_s = to.as_str();
    let now = db::now();
    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE members SET state = ?1, left_at = ?2 WHERE id = ?3",
            params![to_s, now, actor.id],
        )?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "member.left",
            json!({ "display_name": actor.display_name }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("leave failed: {e}")))?;
    Ok(json!({ "ok": true, "member_id": actor.id, "state": to_s, "from": from_s, "team": team.id }))
}

fn cmd_team_archive(conn: &mut Connection, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    let from = TeamState::from_str(&team.state).unwrap_or(TeamState::Completed);
    let to = crate::state::team_transition(from, &Action::ArchiveTeam).map_err(AppError)?;
    let from_s = from.as_str();
    let to_s = to.as_str();
    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE teams SET state = ?1, updated_at = ?2 WHERE id = ?3",
            params![to_s, db::now(), team.id],
        )?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "team.state_changed",
            json!({ "from": from_s, "to": to_s, "kind": "archive" }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("archive failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "team": team.id, "state": to_s, "from": from_s }))
}

/// Soft-destroy a team (owner only): mark it `destroyed`, revoke all pending
/// invitations, and hide it from lists. Data is preserved for audit.
fn cmd_team_destroy(conn: &mut Connection, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    let from = TeamState::from_str(&team.state).unwrap_or(TeamState::Forming);
    let to = crate::state::team_transition(from, &Action::DestroyTeam).map_err(AppError)?;
    let from_s = from.as_str();
    let to_s = to.as_str();
    let now = db::now();
    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE teams SET state = ?1, updated_at = ?2 WHERE id = ?3",
            params![to_s, now, team.id],
        )?;
        // soft-destroy: revoke every outstanding invitation so no one can
        // import a letter and join a destroyed team.
        tx.execute(
            "UPDATE invitations SET revoked_at = ?1 WHERE team_id = ?2 AND revoked_at IS NULL",
            params![now, team.id],
        )?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "team.destroyed",
            json!({ "from": from_s, "team": team.name }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("destroy failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "team": team.id, "state": to_s, "from": from_s, "destroyed": true }))
}

// ---------------------------------------------------------------------------
// Invitation letters (network mode I1)
// ---------------------------------------------------------------------------

/// Derive a usable role key from a human label ("tester" → "tester"; a
/// non-ASCII label falls back to a short `role-<hex>` slug).
fn role_key_from_label(label: &str) -> String {
    let slug: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() || slug.len() > 32 {
        format!("role-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
    } else {
        slug
    }
}

fn invitations_for_team(conn: &Connection, team_id: &str) -> rusqlite::Result<Vec<InvitationRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, team_id, member_id, role_key, role_label, role_desc, cert_serial, cert_cn,
                created_by, created_at, used_by, used_at, revoked_at
         FROM invitations WHERE team_id = ?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([team_id], invitation_row)?;
    rows.collect()
}

/// `team invite "<label>[: <desc>]"` — owner issues a member cert + letter.
fn cmd_team_invite(
    conn: &mut Connection,
    role_desc: &str,
    name_hint: Option<&str>,
    server_url: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;

    let (label, desc) = match role_desc.split_once(':') {
        Some((l, d)) => {
            let d = d.trim();
            (l.trim(), if d.is_empty() { None } else { Some(d.to_string()) })
        }
        None => (role_desc.trim(), None),
    };
    if label.is_empty() {
        return err("invite requires a non-empty role (e.g. `测试工程师: 负责测试并汇报缺陷`)");
    }
    let role_key = role_key_from_label(label);

    let member_id = uuid::Uuid::new_v4().to_string();
    let invitation_id = uuid::Uuid::new_v4().to_string();
    let home = teamx_home_dir();
    let issued = pki::issue_member_cert(&home, &member_id, &role_key).map_err(AppError)?;
    let ca_pem = std::fs::read_to_string(pki::ca_dir(&home).join("ca.crt"))
        .map_err(|e| AppError(format!("read ca cert: {e}")))?;
    let fingerprint = pki::ca_fingerprint(&home).map_err(AppError)?;
    let server = server_url.unwrap_or("https://127.0.0.1:5781").to_string();
    let now = db::now();

    let letter = json!({
        "teamx_invitation": {
            "version": 1,
            "invitation_id": invitation_id,
            "team": { "id": team.id, "name": team.name },
            "server": { "url": server, "ca_fingerprint": format!("sha256:{fingerprint}") },
            "member": { "name_hint": name_hint.unwrap_or("") },
            "role": { "key": role_key, "label": label, "description": desc },
            "issued_at": now,
            "expires_at": null,
        },
        "certificates": {
            "ca_cert": ca_pem,
            "client_cert": issued.cert_pem,
            "client_key": issued.key_pem,
        },
    });

    let label_owned = label.to_string();
    let desc_ref = desc.as_deref();
    db::with_write(conn, |tx| {
        tx.execute(
            "INSERT OR IGNORE INTO roles (team_id, key, label, description, permissions_json, state, proposed_by)
             VALUES (?1, ?2, ?3, ?4, '{}', 'approved', ?5)",
            params![team.id, role_key, label_owned, desc_ref, actor.id],
        )?;
        tx.execute(
            "INSERT INTO invitations (id, team_id, member_id, role_key, role_label, role_desc, cert_serial, cert_cn, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                invitation_id,
                team.id,
                member_id,
                role_key,
                label_owned,
                desc_ref,
                issued.serial_hex,
                issued.cn,
                actor.id,
                now
            ],
        )?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "invitation.created",
            json!({ "invitation_id": invitation_id, "member_id": member_id, "role": role_key, "role_label": label_owned }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("invite failed: {e}")))?;

    let letter_json = serde_json::to_string(&letter).map_err(|e| AppError(format!("serialize letter: {e}")))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(letter_json.as_bytes());

    Ok(json!({
        "ok": true,
        "invitation_id": invitation_id,
        "member_id": member_id,
        "role": { "key": role_key, "label": label, "description": desc },
        "letter": format!("teamx-inv:v1:{encoded}"),
        "note": "share this letter with the member; they import it with `teamx team import <letter>`",
    }))
}

fn cmd_team_invite_list(conn: &Connection, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    let rows = invitations_for_team(conn, &team.id).map_err(|e| AppError(format!("db error: {e}")))?;
    let list: Vec<Value> = rows
        .iter()
        .map(|i| {
            let state = if i.revoked_at.is_some() {
                "revoked"
            } else if i.used_by.is_some() {
                "used"
            } else {
                "unused"
            };
            json!({
                "invitation_id": i.id,
                "member_id": i.member_id,
                "role_key": i.role_key,
                "role_label": i.role_label,
                "state": state,
                "created_at": i.created_at,
                "used_by": i.used_by,
                "revoked_at": i.revoked_at,
            })
        })
        .collect();
    Ok(json!({ "ok": true, "invitations": list }))
}

fn cmd_team_invite_revoke(conn: &mut Connection, id: &str, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    let inv = invitation_by_id(conn, id)?;
    if inv.team_id != team.id {
        return err(format!("invitation {id} is not in team {}", team.id));
    }
    if inv.revoked_at.is_some() {
        return err(format!("invitation {id} is already revoked"));
    }
    let now = db::now();
    db::with_write(conn, |tx| {
        tx.execute("UPDATE invitations SET revoked_at = ?1 WHERE id = ?2", params![now, id])?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "invitation.revoked",
            json!({ "invitation_id": id, "member_id": inv.member_id }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("revoke failed: {e}")))?;
    Ok(json!({ "ok": true, "invitation_id": id, "member_id": inv.member_id, "revoked": true }))
}

/// `team import <letter>` — unpack the letter and store the mTLS material
/// locally. When the invitation exists in the local DB (single-machine mode)
/// the pre-allocated member seat is also claimed (pending).
fn cmd_team_import(
    conn: &mut Connection,
    letter: &str,
    name: Option<&str>,
    session: &str,
    expected_member_id: Option<&str>,
) -> Result<Value> {
    let letter_val = decode_letter(letter)?;
    let inv = &letter_val["teamx_invitation"];
    let invitation_id = inv["invitation_id"]
        .as_str()
        .ok_or_else(|| AppError("invalid letter: missing invitation_id".into()))?;
    let version = inv["version"].as_i64().unwrap_or(0);
    if version != 1 {
        return err(format!("unsupported invitation letter version {version}"));
    }

    let certs = &letter_val["certificates"];
    let ca_pem = certs["ca_cert"].as_str().ok_or_else(|| AppError("letter missing ca_cert".into()))?;
    let client_cert = certs["client_cert"].as_str().ok_or_else(|| AppError("letter missing client_cert".into()))?;
    let client_key = certs["client_key"].as_str().ok_or_else(|| AppError("letter missing client_key".into()))?;

    store_letter(invitation_id, &letter_val, ca_pem, client_cert, client_key)?;

    // The invitation lives on the owner's DB. On a member's own machine it is
    // absent, so a local import only stores the material; registration happens
    // server-side over mTLS (`team.import` RPC).
    let inv_row = match invitation_by_id_opt(conn, invitation_id).map_err(|e| AppError(format!("db error: {e}")))? {
        Some(r) => r,
        None => {
            let server_url = inv["server"]["url"].as_str().unwrap_or("");
            let mut payload = json!({
                "ok": true,
                "status": "stored",
                "invitation_id": invitation_id,
                "letters_dir": teamx_home_dir().join("letters").join(invitation_id).display().to_string(),
                "note": "letter stored locally; the plugin will auto-connect to the server on next start",
            });
            if !server_url.is_empty() {
                payload["server_url"] = json!(server_url);
                payload["note"] = json!(format!(
                    "letter stored locally; auto-connect to {server_url} on next opencode start \
                     (or set TEAMX_SERVER_URL={server_url} now)"
                ));
            }
            return Ok(payload);
        }
    };

    claim_invitation(conn, &inv_row, session, name, &letter_val, expected_member_id)
}

/// Network mode: register a member from their invitation letter, binding the
/// pre-allocated seat to the certificate-derived member id (no local store —
/// the member already stored the letter on their own machine).
pub fn import_with_cert(
    conn: &mut Connection,
    letter: &str,
    name: Option<&str>,
    member_id: &str,
) -> Result<Value> {
    let letter_val = decode_letter(letter)?;
    let inv = &letter_val["teamx_invitation"];
    let invitation_id = inv["invitation_id"]
        .as_str()
        .ok_or_else(|| AppError("invalid letter: missing invitation_id".into()))?;
    let inv_row = invitation_by_id(conn, invitation_id)?;
    let session = format!("net:{member_id}");
    claim_invitation(conn, &inv_row, &session, name, &letter_val, Some(member_id))
}

/// Claim a pre-allocated member seat from a valid invitation row.
fn claim_invitation(
    conn: &mut Connection,
    inv_row: &InvitationRow,
    session: &str,
    name: Option<&str>,
    letter_val: &Value,
    expected_member_id: Option<&str>,
) -> Result<Value> {
    let invitation_id = inv_row.id.clone();
    if let Some(expected) = expected_member_id {
        if inv_row.member_id != expected {
            return err(format!(
                "letter {invitation_id} does not match your certificate identity (cert member {expected}, letter member {})",
                inv_row.member_id
            ));
        }
    }
    if inv_row.revoked_at.is_some() {
        return err(format!("invitation {invitation_id} has been revoked"));
    }
    if inv_row.used_by.as_deref().is_some() && inv_row.used_by.as_deref() != Some(session) {
        return err(format!("invitation {invitation_id} has already been used"));
    }

    let inv = &letter_val["teamx_invitation"];
    let role_key = inv["role"]["key"].as_str().unwrap_or(&inv_row.role_key).to_string();
    let role_label = inv["role"]["label"].as_str();
    let name_hint = inv["member"]["name_hint"].as_str().unwrap_or("");
    let display_name = name
        .filter(|n| !n.is_empty())
        .or_else(|| (!name_hint.is_empty()).then_some(name_hint))
        .unwrap_or_else(|| role_label.unwrap_or("member"))
        .to_string();

    let team_id = inv_row.team_id.clone();
    let member_id = inv_row.member_id.clone();
    let now = db::now();

    let existing: Option<(String, String)> = conn
        .query_row("SELECT id, state FROM members WHERE id = ?1", [&member_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(|e| AppError(format!("db error: {e}")))?;

    match existing {
        Some((_, st)) if st == "left" || st == "denied" => {
            db::with_write(conn, |tx| {
                tx.execute(
                    "UPDATE members SET session_key = ?1, display_name = ?2, role = ?3, state = 'pending',
                     joined_at = ?4, left_at = NULL WHERE id = ?5",
                    params![session, display_name, role_key, now, member_id],
                )?;
                emit_json(
                    tx,
                    &team_id,
                    Some(&member_id),
                    "membership.pending",
                    json!({ "display_name": display_name, "team": team_id, "invitation_id": invitation_id, "rejoined": true }),
                )?;
                Ok(())
            })
            .map_err(|e| AppError(format!("import failed: {e}")))?;
        }
        Some((_, st)) => {
            return err(format!("member {member_id} already exists (state {st}); import once"));
        }
        None => {
            db::with_write(conn, |tx| {
                tx.execute(
                    "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
                    params![member_id, team_id, session, display_name, role_key, now],
                )?;
                emit_json(
                    tx,
                    &team_id,
                    Some(&member_id),
                    "membership.pending",
                    json!({ "display_name": display_name, "team": team_id, "invitation_id": invitation_id, "role": role_key }),
                )?;
                Ok(())
            })
            .map_err(|e| AppError(format!("import failed: {e}")))?;
        }
    }

    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE invitations SET used_by = ?1, used_at = ?2 WHERE id = ?3",
            params![session, now, invitation_id],
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("import failed: {e}")))?;

    let team = team_by_id(conn, &team_id)?;
    Ok(json!({
        "ok": true,
        "status": "pending",
        "member_id": member_id,
        "role": role_key,
        "team": { "id": team.id, "name": team.name, "state": team.state },
        "note": "invitation imported; waiting for owner approval",
    }))
}

/// Decode a letter from `teamx-inv:v1:<base64>`, a file path, or raw JSON.
fn decode_letter(letter: &str) -> Result<Value> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;

    let text = if let Some(b64part) = letter.strip_prefix("teamx-inv:v1:") {
        let bytes = b64
            .decode(b64part)
            .map_err(|e| AppError(format!("invalid letter base64: {e}")))?;
        String::from_utf8(bytes).map_err(|e| AppError(format!("letter is not UTF-8: {e}")))?
    } else if std::path::Path::new(letter).is_file() {
        let bytes = std::fs::read(letter)
            .map_err(|e| AppError(format!("cannot read letter file {letter}: {e}")))?;
        let s = String::from_utf8(bytes).map_err(|e| AppError(format!("letter file is not UTF-8: {e}")))?;
        // a file may itself contain the `teamx-inv:v1:` prefix
        match s.trim().strip_prefix("teamx-inv:v1:") {
            Some(b64part) => {
                let bytes = b64
                    .decode(b64part)
                    .map_err(|e| AppError(format!("invalid letter base64: {e}")))?;
                String::from_utf8(bytes).map_err(|e| AppError(format!("letter is not UTF-8: {e}")))?
            }
            None => s,
        }
    } else {
        letter.to_string()
    };

    serde_json::from_str(&text).map_err(|e| AppError(format!("invalid letter JSON: {e}")))
}

/// Store the unpacked letter + mTLS material under `~/.teamx/letters/<id>/`.
fn store_letter(invitation_id: &str, letter: &Value, ca_pem: &str, client_cert: &str, client_key: &str) -> Result<()> {
    // The invitation_id comes from the (untrusted) letter, so validate it is a
    // UUID before using it as a path component — otherwise a letter could write
    // outside `~/.teamx/letters/` (path traversal).
    if !is_uuid(invitation_id) {
        return err(format!("invalid invitation_id `{invitation_id}` (must be a UUID)"));
    }
    let dir = teamx_home_dir().join("letters").join(invitation_id);
    std::fs::create_dir_all(&dir).map_err(|e| AppError(format!("cannot create {}: {e}", dir.display())))?;
    let write = |name: &str, content: &str| -> Result<()> {
        let p = dir.join(name);
        write_private(&p, content)
    };
    write("letter.json", &serde_json::to_string_pretty(letter).unwrap_or_default())?;
    write("ca.crt", ca_pem)?;
    write("client.crt", client_cert)?;
    write("client.key", client_key)?;
    // convenience pointer so the plugin can discover the most recent import
    let cur = teamx_home_dir().join("letters").join("current.json");
    std::fs::write(&cur, json!({ "invitation_id": invitation_id }).to_string())
        .map_err(|e| AppError(format!("write current.json: {e}")))?;
    chmod_0600(&cur);
    Ok(())
}

fn chmod_0600(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

/// Write a secret file (keys, invitation letters), creating it with mode 0600
/// directly — `write` + `chmod` leaves a 0644 window on unix.
fn write_private(path: &std::path::Path, content: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| AppError(format!("cannot write {}: {e}", path.display())))?;
        f.write_all(content.as_bytes())
            .map_err(|e| AppError(format!("cannot write {}: {e}", path.display())))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content).map_err(|e| AppError(format!("cannot write {}: {e}", path.display())))?;
    }
    Ok(())
}

/// True for a canonical v4-UUID shape (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`).
/// Used to guard against path traversal when an id is used as a path component.
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn cmd_member_set_state(
    conn: &mut Connection,
    state: &str,
    member_opt: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    let target = match member_opt {
        Some(mid) => {
            ensure_owner(conn, &actor, &team)?;
            member_by_id(conn, mid)?
        }
        None => actor.clone(),
    };
    if target.team_id != team.id {
        return err(format!("member {} is not in team {}", target.id, team.id));
    }
    let from = MemberState::from_str(&target.state).unwrap_or(MemberState::Active);
    let action = match state {
        "idle" => Action::MemberIdle,
        "active" => Action::MemberActive,
        _ => return err(format!("invalid member state `{state}` (valid: idle, active)")),
    };
    let to = crate::state::member_transition(from, &action).map_err(AppError)?;
    let from_s = from.as_str();
    let to_s = to.as_str();

    db::with_write(conn, |tx| {
        tx.execute("UPDATE members SET state = ?1 WHERE id = ?2", params![to_s, target.id])?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "member.state_changed",
            json!({ "member_id": target.id, "display_name": target.display_name, "from": from_s, "to": to_s }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("member set-state failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "member_id": target.id, "state": to_s, "from": from_s }))
}

fn cmd_goal_set(
    conn: &mut Connection,
    title: &str,
    body: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    if title.trim().is_empty() {
        return err("goal title cannot be empty");
    }
    let goal_id = cmd_goal_set_inner(conn, &team.id, &actor.id, title, body)?;
    // Reflect the goal's actual state (a `set` on an existing goal updates it
    // in place and never rewinds the state machine).
    let goal_state = team_goal(conn, &team.id)
        .ok()
        .flatten()
        .map(|g| g.state)
        .unwrap_or_else(|| "proposed".to_string());
    Ok(json!({ "ok": true, "goal_id": goal_id, "state": goal_state }))
}

fn cmd_goal_set_inner(
    conn: &mut Connection,
    team_id: &str,
    member_id: &str,
    title: &str,
    body: Option<&str>,
) -> Result<String> {
    let existing: Option<GoalRow> = team_goal(conn, team_id)?;
    match existing {
        Some(g) => {
            let event_type = if g.state == "proposed" { "goal.set" } else { "goal.updated" };
            db::with_write(conn, |tx| {
                tx.execute(
                    "UPDATE goals SET title = ?1, body = ?2, updated_at = ?3 WHERE id = ?4",
                    params![title, body, db::now(), g.id],
                )?;
                emit_json(
                    tx,
                    team_id,
                    Some(member_id),
                    event_type,
                    json!({ "title": title, "state": g.state }),
                )?;
                Ok(())
            })
            .map_err(|e| AppError(format!("goal set failed: {e}")))?;
            Ok(g.id)
        }
        None => {
            let goal_id = uuid::Uuid::new_v4().to_string();
            let now = db::now();
            db::with_write(conn, |tx| {
                tx.execute(
                    "INSERT INTO goals (id, team_id, title, body, state, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 'proposed', ?5, ?5)",
                    params![goal_id, team_id, title, body, now],
                )?;
                tx.execute("UPDATE teams SET goal_id = ?1 WHERE id = ?2", params![goal_id, team_id])?;
                emit_json(tx, team_id, Some(member_id), "goal.set", json!({ "title": title }))?;
                Ok(())
            })
            .map_err(|e| AppError(format!("goal set failed: {e}")))?;
            Ok(goal_id)
        }
    }
}

fn cmd_goal_share(conn: &mut Connection, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    let goal = team_goal(conn, &team.id)?
        .ok_or_else(|| AppError("no goal set yet; use `teamx goal set <title>` first".to_string()))?;
    let goal_from = GoalState::from_str(&goal.state).unwrap_or(GoalState::Proposed);
    let goal_to = crate::state::goal_transition(goal_from, &Action::ShareGoal).map_err(AppError)?;
    let team_from = TeamState::from_str(&team.state).unwrap_or(TeamState::Forming);
    let team_to = crate::state::team_transition(team_from, &Action::ShareGoal).map_err(AppError)?;
    let g_from = goal_from.as_str();
    let g_to = goal_to.as_str();
    let t_from = team_from.as_str();
    let t_to = team_to.as_str();

    db::with_write(conn, |tx| {
        tx.execute("UPDATE goals SET state = ?1, updated_at = ?2 WHERE id = ?3", params![g_to, db::now(), goal.id])?;
        tx.execute("UPDATE teams SET state = ?1, updated_at = ?2 WHERE id = ?3", params![t_to, db::now(), team.id])?;
        emit_json(tx, &team.id, Some(&actor.id), "goal.shared", json!({ "title": goal.title, "state": g_to }))?;
        emit_json(tx, &team.id, Some(&actor.id), "team.state_changed", json!({ "from": t_from, "to": t_to }))?;
        Ok(())
    })
    .map_err(|e| AppError(format!("goal share failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "goal_state": g_to, "team_state": t_to, "from_goal": g_from, "from_team": t_from }))
}

fn cmd_goal_close(conn: &mut Connection, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    let goal = team_goal(conn, &team.id)?
        .ok_or_else(|| AppError("no goal set yet".to_string()))?;
    let goal_from = GoalState::from_str(&goal.state).unwrap_or(GoalState::Proposed);
    let goal_to = crate::state::goal_transition(goal_from, &Action::CloseGoal).map_err(AppError)?;
    let team_from = TeamState::from_str(&team.state).unwrap_or(TeamState::Forming);
    let team_to = crate::state::team_transition(team_from, &Action::CloseGoal).map_err(AppError)?;
    let g_from = goal_from.as_str();
    let g_to = goal_to.as_str();
    let t_to = team_to.as_str();
    let now = db::now();

    db::with_write(conn, |tx| {
        tx.execute("UPDATE goals SET state = ?1, updated_at = ?2 WHERE id = ?3", params![g_to, now, goal.id])?;
        tx.execute("UPDATE teams SET state = ?1, updated_at = ?2 WHERE id = ?3", params![t_to, now, team.id])?;
        emit_json(tx, &team.id, Some(&actor.id), "goal.state_changed", json!({ "from": g_from, "to": g_to, "kind": "close" }))?;
        emit_json(tx, &team.id, Some(&actor.id), "team.completed", json!({ "goal": goal.title }))?;
        Ok(())
    })
    .map_err(|e| AppError(format!("goal close failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "goal_state": g_to, "team_state": t_to }))
}

fn cmd_role_list(conn: &Connection, team_opt: Option<&str>) -> Result<Value> {
    let roles = match team_opt {
        Some(tid) => roles_json(conn, tid).map_err(|e| AppError(format!("db error: {e}")))?,
        None => DEFAULT_ROLES
            .iter()
            .map(|(key, label, desc)| json!({ "key": key, "label": label, "description": desc }))
            .collect(),
    };
    Ok(json!({ "roles": roles }))
}

fn roles_json(conn: &Connection, team_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT key, label, description, state, proposed_by FROM roles WHERE team_id = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([team_id], |r| {
        Ok(json!({
            "key": r.get::<_, String>(0)?,
            "label": r.get::<_, String>(1)?,
            "description": r.get::<_, Option<String>>(2)?,
            "state": r.get::<_, String>(3)?,
            "proposed_by": r.get::<_, Option<String>>(4)?,
        }))
    })?;
    rows.collect()
}

fn cmd_role_set(
    conn: &mut Connection,
    role: &str,
    session: &str,
    member_opt: Option<&str>,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    let target = match member_opt {
        Some(mid) => {
            ensure_owner(conn, &actor, &team)?;
            member_by_id(conn, mid)?
        }
        None => actor.clone(),
    };
    if target.team_id != team.id {
        return err(format!("member {} is not in team {}", target.id, team.id));
    }
    // The `owner` role is reserved for the actual owner member: only the owner
    // can carry it, and it cannot be self-granted (prevents role spoofing that
    // the plugin's owner detection would otherwise trust).
    if role == "owner" && team.owner_member_id.as_deref() != Some(target.id.as_str()) {
        return err(format!(
            "role `owner` is reserved for the team lead (member {})",
            team.owner_member_id.as_deref().unwrap_or("<none>")
        ));
    }
    let exists = role_approved(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))?;
    if !exists {
        // Distinguish a missing role from one still awaiting owner approval.
        if role_exists(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))? {
            return err(format!(
                "role `{role}` is pending owner approval and cannot be used yet; ask the owner to `role approve {role}`"
            ));
        }
        return err(format!(
            "role `{role}` is not in the team catalog; see `teamx role list --team {}`",
            team.id
        ));
    }
    let label = role_label(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))?.unwrap_or_else(|| role.to_string());
    let from = MemberState::from_str(&target.state).unwrap_or(MemberState::Pending);
    let to = crate::state::member_transition(from, &Action::SetRole).map_err(AppError)?;
    let from_s = from.as_str();
    let to_s = to.as_str();

    db::with_write(conn, |tx| {
        tx.execute("UPDATE members SET role = ?1, state = ?2 WHERE id = ?3", params![role, to_s, target.id])?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "member.role_set",
            json!({ "member_id": target.id, "display_name": target.display_name, "role": role, "label": label }),
        )?;
        if from_s != to_s {
            emit_json(
                tx,
                &team.id,
                Some(&target.id),
                "member.state_changed",
                json!({ "from": from_s, "to": to_s }),
            )?;
        }
        Ok(())
    })
    .map_err(|e| AppError(format!("role set failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "member_id": target.id, "role": role, "label": label, "state": to_s, "from": from_s }))
}

/// Member proposes a custom role (key + label + job description). It lands in
/// state=proposed and only becomes usable after the owner approves it.
fn cmd_role_propose(
    conn: &mut Connection,
    role: &str,
    label: &str,
    description: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    let key = role.trim();
    if key.is_empty() {
        return err("role key cannot be empty");
    }
    let label = label.trim();
    if label.is_empty() {
        return err("role label cannot be empty");
    }
    if DEFAULT_ROLES.iter().any(|(k, _, _)| *k == key) {
        return err(format!(
            "role `{key}` conflicts with a built-in role; pick a different key"
        ));
    }
    if role_exists(conn, &team.id, key).map_err(|e| AppError(format!("db error: {e}")))? {
        let st = role_state(conn, &team.id, key).map_err(|e| AppError(format!("db error: {e}")))?;
        let state_note = match st.as_deref() {
            Some("proposed") => " (already pending approval)",
            Some("approved") => " (already approved)",
            _ => "",
        };
        return err(format!("role `{key}` already exists{state_note}"));
    }

    db::with_write(conn, |tx| {
        tx.execute(
            "INSERT INTO roles (team_id, key, label, description, permissions_json, state, proposed_by)
             VALUES (?1, ?2, ?3, ?4, '{}', 'proposed', ?5)",
            params![team.id, key, label, description, actor.id],
        )?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "role.proposed",
            json!({ "key": key, "label": label, "description": description, "proposed_by": actor.id, "proposer": actor.display_name }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("role propose failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "key": key, "label": label, "state": "proposed", "proposed_by": actor.id, "note": "pending owner approval" }))
}

/// Owner approves a proposed custom role; the proposer is granted the role
/// immediately (state proposed -> approved on the role and member.role set).
fn cmd_role_approve(
    conn: &mut Connection,
    role: &str,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    if !role_exists(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))? {
        return err(format!("role `{role}` does not exist"));
    }
    let st = role_state(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))?.unwrap_or_default();
    if st != "proposed" {
        return err(format!("role `{role}` is already {st}; only proposed roles can be approved"));
    }
    let label = role_label(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))?.unwrap_or_else(|| role.to_string());
    let proposer: Option<String> = conn
        .query_row(
            "SELECT proposed_by FROM roles WHERE team_id = ?1 AND key = ?2",
            params![team.id, role],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| AppError(format!("db error: {e}")))?;
    // Resolve the proposer's member row before entering the write transaction.
    let proposer_member: Option<MemberRow> = match &proposer {
        Some(pid) => member_by_id(conn, pid).ok().filter(|m| m.team_id == team.id && m.state != "left" && m.state != "denied"),
        None => None,
    };

    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE roles SET state = 'approved', proposed_by = NULL WHERE team_id = ?1 AND key = ?2",
            params![team.id, role],
        )?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "role.approved",
            json!({ "key": role, "label": label, "approved_by": actor.id, "approver": actor.display_name }),
        )?;
        // Auto-grant the role to the proposer if they are still active in the team.
        if let Some(member) = &proposer_member {
            tx.execute(
                "UPDATE members SET role = ?1 WHERE id = ?2",
                params![role, member.id],
            )?;
            emit_json(
                tx,
                &team.id,
                Some(&member.id),
                "member.role_set",
                json!({ "member_id": member.id, "display_name": member.display_name, "role": role, "label": label }),
            )?;
        }
        Ok(())
    })
    .map_err(|e| AppError(format!("role approve failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "key": role, "label": label, "state": "approved", "granted_to": proposer_member.map(|m| m.id) }))
}

/// Owner denies a proposed custom role (removes the proposal).
fn cmd_role_deny(
    conn: &mut Connection,
    role: &str,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    if !role_exists(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))? {
        return err(format!("role `{role}` does not exist"));
    }
    let st = role_state(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))?.unwrap_or_default();
    if st != "proposed" {
        return err(format!("role `{role}` is already {st}; only proposed roles can be denied"));
    }
    let label = role_label(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))?.unwrap_or_else(|| role.to_string());

    db::with_write(conn, |tx| {
        tx.execute(
            "DELETE FROM roles WHERE team_id = ?1 AND key = ?2",
            params![team.id, role],
        )?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "role.denied",
            json!({ "key": role, "label": label, "denied_by": actor.id, "denier": actor.display_name }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("role deny failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "key": role, "label": label, "state": "denied" }))
}

/// Owner updates a role's label/description (built-in or custom).
fn cmd_role_update(
    conn: &mut Connection,
    role: &str,
    label_opt: Option<&str>,
    description_opt: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    if !role_exists(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))? {
        return err(format!("role `{role}` does not exist"));
    }
    let old_label = role_label(conn, &team.id, role).map_err(|e| AppError(format!("db error: {e}")))?.unwrap_or_else(|| role.to_string());
    let new_label = label_opt.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&old_label).to_string();
    // Preserve the existing description when --description is not provided.
    let old_desc: Option<String> = conn
        .query_row(
            "SELECT description FROM roles WHERE team_id = ?1 AND key = ?2",
            params![team.id, role],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|e| AppError(format!("db error: {e}")))?
        .flatten();
    let desc: Option<String> = match description_opt {
        Some(s) => {
            let t = s.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        }
        None => old_desc,
    };

    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE roles SET label = ?1, description = ?2 WHERE team_id = ?3 AND key = ?4",
            params![new_label, desc, team.id, role],
        )?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "role.updated",
            json!({ "key": role, "label": new_label, "description": desc, "updated_by": actor.id }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("role update failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "key": role, "label": new_label, "description": desc }))
}

// Publish type -> (Action, event type) + goal/team transition policy
fn publish_plan(publish_type: &str) -> Result<(Action, &'static str, bool, bool)> {
    // (action, event type, affects goal, affects team)
    Ok(match publish_type {
        "start" => (Action::PublishStart, "goal.state_changed", true, false),
        "progress" => (Action::PublishProgress, "progress.published", true, false),
        "activity" => (Action::PublishProgress, "progress.published", false, false),
        "decision" => (Action::PublishDecision, "decision.broadcast", false, false),
        "update" => (Action::PublishDecision, "decision.broadcast", false, false),
        "blocked" => (Action::PublishBlocked, "goal.state_changed", true, true),
        "resumed" => (Action::PublishResumed, "goal.state_changed", true, true),
        "achieved" => (Action::PublishAchieved, "goal.achieved", true, false),
        "refine" => (Action::PublishRefine, "goal.state_changed", true, false),
        other => {
            return err(format!(
                "unknown publish type `{other}` (valid: start, progress, activity, decision, update, blocked, resumed, achieved, refine)"
            ))
        }
    })
}

fn cmd_publish(
    conn: &mut Connection,
    publish_type: &str,
    data: Option<&str>,
    assignee_opt: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (action, event_type, affects_goal, affects_team) = publish_plan(publish_type)?;
    let (actor, team) = resolve_actor(conn, session, team_opt)?;

    // A pending member is not yet an approved collaborator and must not publish
    // events (neither state changes nor broadcasts). Waiting/idle members are
    // still active and may publish.
    if actor.state == "pending" {
        return err(format!(
            "member {} is pending; wait for owner approval before publishing",
            actor.display_name
        ));
    }

    // Resolve the assignee (if any): must be an active member of this team.
    let assignee = match assignee_opt {
        Some(aid) => {
            let m = member_by_id(conn, aid)?;
            if m.team_id != team.id {
                return err(format!("assignee member {aid} is not in team {}", team.id));
            }
            if m.state == "left" || m.state == "denied" {
                return err(format!("assignee member {} is not active (state: {})", m.display_name, m.state));
            }
            Some((aid.to_string(), m.display_name))
        }
        None => None,
    };

    let mut payload: Value = match data {
        // Accept a bare (non-JSON) string as `{"message": s}` for robustness
        // against model tool calls that pass a plain sentence.
        Some(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({ "message": s })),
        None => json!({}),
    };
    // Normalize non-object payloads (arrays/strings/numbers) before tagging, so
    // `payload["assignee_member_id"] = ...` can never panic on a non-object.
    if !payload.is_object() {
        payload = json!({ "message": payload });
    }
    // Tag the event with the assignee so the plugin can auto-execute on that
    // member only (and everyone else treats it as informational).
    if let Some((aid, name)) = &assignee {
        payload["assignee_member_id"] = json!(aid);
        payload["assignee_name"] = json!(name);
    }
    let goal = team_goal(conn, &team.id)?;
    if affects_goal && goal.is_none() {
        return err("no goal set yet; use `teamx goal set <title>` first");
    }

    // Validate transitions BEFORE writing anything. Only types that actually
    // affect a state (affects_goal/affects_team) attempt a transition; neutral
    // broadcasts (decision/update/activity) never touch goal or team state.
    let goal_transition: Option<(String, String)> = if affects_goal {
        match &goal {
            Some(g) => {
                let from = GoalState::from_str(&g.state).unwrap_or(GoalState::Proposed);
                Some((from.as_str().to_string(), crate::state::goal_transition(from, &action).map_err(AppError)?.as_str().to_string()))
            }
            None => None,
        }
    } else {
        None
    };
    let team_transition: Option<(String, String)> = if affects_team {
        let from = TeamState::from_str(&team.state).unwrap_or(TeamState::Forming);
        Some((from.as_str().to_string(), crate::state::team_transition(from, &action).map_err(AppError)?.as_str().to_string()))
    } else {
        None
    };

    let now = db::now();
    let seq = db::with_write(conn, |tx| {
        let seq = emit_json(tx, &team.id, Some(&actor.id), event_type, payload.clone())?;
        if let Some((_, to)) = &goal_transition {
            if let Some(g) = &goal {
                tx.execute("UPDATE goals SET state = ?1, updated_at = ?2 WHERE id = ?3", params![to, now, g.id])?;
            }
        }
        if let Some((_, to)) = &team_transition {
            tx.execute("UPDATE teams SET state = ?1, updated_at = ?2 WHERE id = ?3", params![to, now, team.id])?;
        }
        Ok(seq)
    })
    .map_err(|e| AppError(format!("publish failed: {e}")))?;
    touch(conn, &actor.id).ok();

    Ok(json!({
        "ok": true,
        "seq": seq,
        "event": event_type,
        "type": publish_type,
        "assignee": assignee.map(|(id, _)| id),
        "goal_state": goal_transition.map(|(_, t)| t),
        "team_state": team_transition.map(|(_, t)| t),
    }))
}

fn open_questions(conn: &Connection, team_id: &str) -> rusqlite::Result<Vec<QuestionRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, team_id, asker_member_id, target_member_id, question, answer, state, created_at
         FROM questions WHERE team_id = ?1 ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([team_id], question_row)?;
    rows.collect()
}

fn cmd_ask(
    conn: &mut Connection,
    member_id: &str,
    question: &str,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    let target = member_by_id(conn, member_id)?;
    if target.team_id != team.id {
        return err(format!("member {member_id} is not in team {}", team.id));
    }
    if target.id == actor.id {
        return err("cannot ask yourself a question");
    }
    let from = MemberState::from_str(&target.state).unwrap_or(MemberState::Active);
    let to = crate::state::member_transition(from, &Action::Ask).map_err(AppError)?;
    if to != MemberState::Waiting {
        return err(format!("member {} is in state {} and cannot be asked", target.display_name, target.state));
    }
    let question_id = uuid::Uuid::new_v4().to_string();
    let target_id = target.id.clone();
    let target_name = target.display_name.clone();
    let now = db::now();
    db::with_write(conn, |tx| {
        tx.execute(
            "INSERT INTO questions (id, team_id, asker_member_id, target_member_id, question, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6)",
            params![question_id, team.id, actor.id, target_id, question, now],
        )?;
        tx.execute("UPDATE members SET state = 'waiting' WHERE id = ?1", params![target.id])?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "clarification.asked",
            json!({ "question_id": question_id, "target_member_id": target_id, "target": target_name, "question": question }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("ask failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "question_id": question_id, "target_member_id": target_id, "target_state": "waiting" }))
}

fn cmd_respond(conn: &mut Connection, ask_id: &str, answer: &str, session: &str) -> Result<Value> {
    let q: QuestionRow = conn
        .query_row(
            "SELECT id, team_id, asker_member_id, target_member_id, question, answer, state, created_at
             FROM questions WHERE id = ?1",
            [ask_id],
            question_row,
        )
        .map_err(|e| AppError(format!("question {ask_id} not found: {e}")))?;
    if q.state != "open" {
        return err(format!("question {ask_id} is already {}", q.state));
    }
    let (actor, _team) = resolve_actor(conn, session, Some(&q.team_id))?;
    if actor.id != q.target_member_id {
        return err("only the target member may respond to this question");
    }
    let target_from = MemberState::Waiting;
    let target_to = crate::state::member_transition(target_from, &Action::Respond).map_err(AppError)?;
    let now = db::now();
    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE questions SET answer = ?1, state = 'answered', answered_at = ?2 WHERE id = ?3",
            params![answer, now, q.id],
        )?;
        tx.execute("UPDATE members SET state = 'active' WHERE id = ?1", params![q.target_member_id])?;
        emit_json(
            tx,
            &q.team_id,
            Some(&actor.id),
            "clarification.responded",
            json!({ "question_id": q.id, "question": q.question, "answer": answer }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("respond failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "question_id": q.id, "target_state": target_to.as_str(), "from": target_from.as_str() }))
}

fn cmd_events(conn: &Connection, after: Option<i64>, team_opt: Option<&str>) -> Result<Value> {
    let team_id = team_opt.ok_or_else(|| AppError("teamx events requires --team <id>".to_string()))?;
    team_by_id(conn, team_id)?;
    let events = events::list(conn, team_id, after).map_err(|e| AppError(format!("db error: {e}")))?;
    Ok(json!({ "team_id": team_id, "events": events.iter().map(event_json).collect::<Vec<_>>() }))
}

/// Human-readable audit replay: resolves member display names and returns the
/// timeline (optionally capped to the last `limit` events).
fn cmd_log(
    conn: &Connection,
    team_opt: Option<&str>,
    session_opt: Option<&str>,
    limit: Option<i64>,
    after: Option<i64>,
) -> Result<Value> {
    let team_id = match team_opt {
        Some(t) => t.to_string(),
        None => {
            let session = session_opt
                .ok_or_else(|| AppError("teamx log requires --team <id> or --session <key>".to_string()))?;
            let members = memberships_for_session(conn, session)
                .map_err(|e| AppError(format!("db error: {e}")))?;
            if members.is_empty() {
                return err(format!("session `{session}` is not a member of any team"));
            }
            if members.len() > 1 {
                let list: Vec<String> = members.iter().map(|m| m.team_id.clone()).collect();
                return err(format!("session `{session}` belongs to multiple teams; pass --team (one of {list:?})"));
            }
            members[0].team_id.clone()
        }
    };
    let team = team_by_id(conn, &team_id)?;
    let mut events = events::list(conn, &team_id, after).map_err(|e| AppError(format!("db error: {e}")))?;
    let members = members_for_team(conn, &team_id).map_err(|e| AppError(format!("db error: {e}")))?;
    let names: std::collections::HashMap<String, String> = members
        .iter()
        .map(|m| (m.id.clone(), m.display_name.clone()))
        .collect();
    if let Some(lim) = limit {
        let lim = lim.max(0) as usize;
        if events.len() > lim {
            events.drain(0..events.len() - lim);
        }
    }
    let timeline: Vec<Value> = events
        .iter()
        .map(|e| {
            json!({
                "seq": e.seq,
                "type": e.r#type,
                "member": e.member_id.as_ref().and_then(|id| names.get(id)).cloned(),
                "member_id": e.member_id,
                "payload": e.payload,
                "created_at": e.created_at,
            })
        })
        .collect();
    Ok(json!({ "team": { "id": team.id, "name": team.name }, "events": timeline }))
}

fn event_json(e: &events::Event) -> Value {
    json!({
        "seq": e.seq,
        "team_id": e.team_id,
        "member_id": e.member_id,
        "type": e.r#type,
        "payload": e.payload,
        "created_at": e.created_at,
    })
}

/// Highest global event row id (used by the server to detect newly-written
/// events after a command and broadcast them to live WS connections).
pub fn max_event_id(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))
}

/// Events written after `after_id` (global autoincrement id), each serialized
/// as a JSON object that includes `team_id`, for WS fan-out.
pub fn events_after(conn: &Connection, after_id: i64) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, team_id, member_id, seq, type, payload_json, created_at
         FROM events WHERE id > ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([after_id], |r| {
        let payload_json: Option<String> = r.get(5)?;
        let payload: Option<Value> = payload_json.and_then(|s| serde_json::from_str(&s).ok());
        Ok(json!({
            "team_id": r.get::<_, String>(1)?,
            "member_id": r.get::<_, Option<String>>(2)?,
            "seq": r.get::<_, i64>(3)?,
            "type": r.get::<_, String>(4)?,
            "payload": payload,
            "created_at": r.get::<_, String>(6)?,
        }))
    })?;
    rows.collect()
}

/// Team ids a member (by id) currently belongs to, for WS subscription.
pub fn teams_for_member(conn: &Connection, member_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT team_id FROM members WHERE id = ?1 AND state NOT IN ('left','denied')",
    )?;
    let rows = stmt.query_map([member_id], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// True if this member's invitation letter has been revoked (network mode I2).
/// Only invitation-issued members have an `invitations` row; token-joined
/// members are not affected by invitation revocation.
pub fn is_revoked(conn: &Connection, member_id: &str) -> rusqlite::Result<bool> {
    let revoked: Option<i64> = conn
        .query_row(
            "SELECT COUNT(*) FROM invitations WHERE member_id = ?1 AND revoked_at IS NOT NULL",
            [member_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(revoked.unwrap_or(0) > 0)
}

/// True if a member (by id) is a non-left/denied member of the given team.
/// Used to enforce team leadership on cross-team reads in network mode.
pub fn member_in_team(conn: &Connection, member_id: &str, team_id: &str) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM members WHERE id = ?1 AND team_id = ?2 AND state NOT IN ('left','denied')",
        params![member_id, team_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn member_json(m: &MemberRow) -> Value {
    json!({
        "id": m.id,
        "display_name": m.display_name,
        "role": m.role,
        "state": m.state,
        "loopx_project": m.loopx_project,
        "joined_at": m.joined_at,
    })
}

fn question_json(q: &QuestionRow) -> Value {
    json!({
        "id": q.id,
        "asker_member_id": q.asker_member_id,
        "target_member_id": q.target_member_id,
        "question": q.question,
        "answer": q.answer,
        "state": q.state,
        "created_at": q.created_at,
    })
}

fn cmd_sync(conn: &mut Connection, session: &str, no_advance: bool) -> Result<Value> {
    let members = memberships_for_session(conn, session).map_err(|e| AppError(format!("db error: {e}")))?;
    let mut teams_out = Vec::new();
    let mut new_events = Vec::new();
    for m in members {
        let team = team_by_id(conn, &m.team_id)?;
        let cursor = events::cursor_for(conn, session, &team.id).map_err(|e| AppError(format!("db error: {e}")))?;
        let events = events::list(conn, &team.id, Some(cursor)).map_err(|e| AppError(format!("db error: {e}")))?;
        let last_seq = events.iter().map(|e| e.seq).max().unwrap_or(cursor);
        if !no_advance {
            db::with_write(conn, |tx| {
                events::set_cursor(tx, session, &team.id, last_seq)
            })
            .map_err(|e| AppError(format!("db error: {e}")))?;
        }
        let mut status = team_status_json(conn, &team)?;
        status["team"]["my_role"] = Value::from(m.role.clone());
        status["team"]["my_state"] = Value::from(m.state.clone());
        status["team"]["my_member_id"] = Value::from(m.id.clone());
        teams_out.push(status);
        new_events.extend(events.iter().map(event_json));
    }
    if teams_out.is_empty() {
        return err(format!(
            "session `{session}` is not a member of any team. Join one first (teamx team join <token> ...)."
        ));
    }
    new_events.sort_by(|a, b| a["seq"].as_i64().unwrap_or(0).cmp(&b["seq"].as_i64().unwrap_or(0)));
    Ok(json!({ "teams": teams_out, "new_events": new_events }))
}

fn cmd_loopx_report(
    conn: &mut Connection,
    project: &std::path::Path,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    let digest = loopx::loopx_status(project);
    if !digest.available {
        return Ok(json!({
            "ok": false,
            "loopx": digest,
            "note": "loopx unavailable; teamx core loop is unaffected",
        }));
    }
    let payload = serde_json::to_value(&digest)
        .map_err(|e| AppError(format!("serialize digest failed: {e}")))?;
    let seq = db::with_write(conn, |tx| {
        emit_json(tx, &team.id, Some(&actor.id), "loopx.progress", payload.clone())
    })
    .map_err(|e| AppError(format!("loopx report failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "seq": seq, "loopx": digest }))
}

// ---------------------------------------------------------------------------
// PKI (mTLS certificates)
// ---------------------------------------------------------------------------

fn teamx_home_dir() -> std::path::PathBuf {
    db::teamx_home()
}

/// `teamx cert init` — ensure the instance CA + server cert exist.
fn cmd_cert_init() -> Result<Value> {
    let home = teamx_home_dir();
    let pk = pki::ensure_pki(&home).map_err(AppError)?;
    Ok(json!({
        "ok": true,
        "ca_cert": pk.ca_cert.display().to_string(),
        "server_cert": pk.server_cert.display().to_string(),
        "note": "instance CA + server certificate ready",
    }))
}

/// `teamx cert issue <member_id> <role> [--out dir]` — issue a member cert.
fn cmd_cert_issue(member_id: &str, role: &str, out: Option<&std::path::Path>) -> Result<Value> {
    let home = teamx_home_dir();
    let issued = pki::issue_member_cert(&home, member_id, role).map_err(AppError)?;
    let cn = &issued.cn;
    match out {
        Some(dir) => {
            std::fs::create_dir_all(dir).map_err(|e| AppError(format!("cannot create {}: {e}", dir.display())))?;
            let cert_path = dir.join("member.crt");
            let key_path = dir.join("member.key");
            write_private(&cert_path, &issued.cert_pem)?;
            write_private(&key_path, &issued.key_pem)?;
            Ok(json!({
                "ok": true,
                "cn": cn,
                "serial": issued.serial_hex,
                "cert": cert_path.display().to_string(),
                "key": key_path.display().to_string(),
            }))
        }
        None => Ok(json!({ "ok": true, "cn": cn, "serial": issued.serial_hex, "cert_pem": issued.cert_pem, "key_pem": issued.key_pem })),
    }
}

/// `teamx cert ca` — print the CA certificate PEM.
fn cmd_cert_ca() -> Result<Value> {
    let home = teamx_home_dir();
    let pk = pki::ensure_pki(&home).map_err(AppError)?;
    let ca_pem = std::fs::read_to_string(&pk.ca_cert).map_err(|e| AppError(format!("read ca: {e}")))?;
    Ok(json!({ "ok": true, "ca_pem": ca_pem, "path": pk.ca_cert.display().to_string() }))
}
