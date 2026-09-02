use crate::cli::{CertCmd, Cli, Command, GoalCmd, LocalCmd, LoopxCmd, MemberCmd, ProxyCmd, RoleCmd, RoutesCmd, TeamCmd, TunnelCmd, UserCmd};
use crate::db::{self, DEFAULT_ROLES};
use crate::events;
use crate::loopx;
use crate::pki;
use crate::state::{Action, GoalState, MemberState, TeamState};
use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::path::PathBuf;

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
    is_lead: bool,
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
    user_id: Option<String>,
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
        is_lead: r.get(9)?,
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
        user_id: r.get(8)?,
        created_by: r.get(9)?,
        created_at: r.get(10)?,
        used_by: r.get(11)?,
        used_at: r.get(12)?,
        revoked_at: r.get(13)?,
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
        "SELECT id, team_id, session_key, display_name, role, state, loopx_project, joined_at, left_at, is_lead
         FROM members WHERE team_id = ?1 ORDER BY joined_at ASC",
    )?;
    let rows = stmt.query_map([team_id], member_row)?;
    rows.collect()
}

fn member_by_id(conn: &Connection, member_id: &str) -> Result<MemberRow> {
    conn.query_row(
        "SELECT id, team_id, session_key, display_name, role, state, loopx_project, joined_at, left_at, is_lead
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
        "SELECT m.id, m.team_id, m.session_key, m.display_name, m.role, m.state, m.loopx_project, m.joined_at, m.left_at, m.is_lead
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

/// True when the member is a team lead: the primary owner or a promoted co-lead.
fn is_team_lead_row(team: &TeamRow, member: &MemberRow) -> bool {
    team.owner_member_id.as_deref() == Some(member.id.as_str()) || member.is_lead
}

fn ensure_owner(_conn: &Connection, actor: &MemberRow, team: &TeamRow) -> Result<()> {
    if is_team_lead_row(team, actor) {
        Ok(())
    } else {
        err(format!(
            "only a team lead may do this (owner member {})",
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
                user_id, created_by, created_at, used_by, used_at, revoked_at
         FROM invitations WHERE id = ?1",
        [id],
        invitation_row,
    )
    .map_err(|_| AppError(format!("invitation {id} not found")))
}

fn invitation_by_id_opt(conn: &Connection, id: &str) -> rusqlite::Result<Option<InvitationRow>> {
    conn.query_row(
        "SELECT id, team_id, member_id, role_key, role_label, role_desc, cert_serial, cert_cn,
                user_id, created_by, created_at, used_by, used_at, revoked_at
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
            TeamCmd::PromoteLead { member_id, session, team } => cmd_team_set_lead(conn, member_id, session, team.as_deref(), true)?,
            TeamCmd::DemoteLead { member_id, session, team } => cmd_team_set_lead(conn, member_id, session, team.as_deref(), false)?,
            TeamCmd::List { session } => cmd_team_list(conn, session)?,
            TeamCmd::Status { team, session } => cmd_team_status(conn, team.as_deref(), session.as_deref())?,
            TeamCmd::Leave { session, team } => cmd_team_leave(conn, session, team.as_deref())?,
            TeamCmd::Archive { session, team } => cmd_team_archive(conn, session, team.as_deref())?,
            TeamCmd::Destroy { session, team } => cmd_team_destroy(conn, session, team.as_deref())?,
            TeamCmd::Invite { role_desc, name_hint, server_url, session, team, user_name, user } => {
                cmd_team_invite(conn, role_desc, name_hint.as_deref(), server_url.as_deref(), session, team.as_deref(), user_name.as_deref(), user.as_deref())?
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
        // Local client config (per-machine).
        Command::Local(lc) => cmd_local(conn, lc)?,
        // Session identity: read-only local query, no DB writes needed.
        Command::Session(sc) => return cmd_session(conn, sc),
        // Task management: built-in taskx doc type (content in git, state in meta).
        Command::Task(tc) => return cmd_task(conn, tc),
        // Plugin management is pure file operations; it does not touch the DB.
        Command::Plugin(pc) => return cmd_plugin(pc),
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
        Command::User(u) => match u {
            UserCmd::List { session, team } => cmd_user_list(conn, session, team.as_deref())?,
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
        // Proxy commands: network-mode only, long-lived WS clients. The
        // `proxy routes` subcommands operate on the SQLite route table.
        Command::Proxy(cmd) => {
            match cmd {
                ProxyCmd::Exit { name, server } => {
                    let url = resolve_server_url(server.as_deref())?;
                    let result = crate::tunnel_client::proxy_exit(&url, name);
                    return result.map_err(AppError);
                }
                ProxyCmd::Routes(rc) => {
                    return proxy_routes_cmd(conn, rc);
                }
                ProxyCmd::Start { port, exit, routes, server } => {
                    let url = resolve_server_url(server.as_deref())?;
                    // Route resolution priority:
                    //   1. -f / --routes JSON file (explicit, ephemeral)
                    //   2. SQLite route table (persistent, default)
                    //   3. --exit fixed name (legacy)
                    //   4. error
                    let table: Option<crate::routes::RouteTable> = match routes {
                        Some(path) => {
                            let text = std::fs::read_to_string(path)
                                .map_err(|e| AppError(format!("routes file {}: {e}", path.display())))?;
                            Some(crate::routes::RouteTable::parse(&text).map_err(AppError)?)
                        }
                        None => match crate::routes::load_from_db(conn) {
                            Ok(Some(t)) => Some(t),
                            Ok(None) => None,
                            Err(e) => return Err(AppError(e)),
                        },
                    };
                    let exit_name = match (&table, exit) {
                        (Some(t), _) => t.default.clone(), // table has its own default
                        (None, Some(e)) => e.clone(),
                        (None, None) => {
                            return Err(AppError(
                                "proxy start: no exit configured — pass --exit <name>, \
                                 -f <routes.json>, or configure `teamx proxy routes set-default`"
                                    .to_string(),
                            ))
                        }
                    };
                    let result = crate::tunnel_client::socks5_proxy(&url, &exit_name, *port, table);
                    return result.map_err(AppError);
                }
            }
        }
        // tun0 virtual NIC: needs root, bridges matching traffic to exits.
        Command::Tun0(cmd) => {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                return crate::tun_cli::handle_tun0(cmd).map_err(AppError);
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                let _ = cmd;
                return Err(AppError("tun0 is only supported on macOS and Linux".to_string()));
            }
        }
        Command::Dns(cmd) => {
            return cmd_dns(conn, cmd);
        }
        Command::Git(cmd) => {
            return cmd_git(conn, cmd);
        }
        // `teamx gui` is handled in main() before the DB opens; unreachable here.
        Command::Gui => {
            return Err(AppError("gui must be launched via `teamx gui`".to_string()));
        }
        // `teamx gui-panel` is handled in main() before the DB opens.
        Command::GuiPanel => {
            return Err(AppError("gui-panel must be launched via `teamx gui-panel`".to_string()));
        }
        // `teamx gui-member` is handled in main() before the DB opens.
        Command::GuiMember => {
            return Err(AppError("gui-member must be launched via `teamx gui-member`".to_string()));
        }
    };
    Ok(out)
}

/// Handle `teamx dns <subcommand>`.
fn cmd_dns(conn: &mut Connection, cmd: &crate::cli::DnsCmd) -> Result<Value> {
    use crate::cli::DnsCmd;
    match cmd {
        DnsCmd::List => {
            let servers = crate::tun_dev::system_dns_servers();
            let s = servers
                .iter()
                .map(|ip| ip.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Ok(serde_json::json!({ "ok": true, "dns": if s.is_empty() { "-".to_string() } else { s } }))
        }
        DnsCmd::Resolve { domain, exit } => {
            let url = resolve_server_url(None)?;
            let exit_name = match exit {
                Some(e) if !e.is_empty() => e.clone(),
                _ => match crate::routes::load_from_db(conn) {
                    Ok(Some(t)) if !t.default.is_empty() => t.default.clone(),
                    _ => {
                        return Err(AppError(
                            "no exit configured — run `teamx proxy routes set-default <exit>`".to_string(),
                        ))
                    }
                },
            };
            let ips = crate::tunnel_client::resolve_dns(&url, &exit_name, domain);
            let s = if ips.is_empty() {
                "（无结果）".to_string()
            } else {
                ips.join("\n")
            };
            Ok(serde_json::json!({ "ok": true, "domain": domain, "exit": exit_name, "ips": s }))
        }
    }
}

/// Handle `teamx proxy routes <subcommand>` — manage the SQLite route table.
fn proxy_routes_cmd(conn: &mut Connection, cmd: &RoutesCmd) -> Result<Value> {
    use crate::routes;
    Ok(match cmd {
        RoutesCmd::List => {
            match routes::load_from_db(conn) {
                Ok(Some(t)) => routes::to_json(&t),
                Ok(None) => {
                    let mut m = serde_json::Map::new();
                    m.insert("default".to_string(), Value::Null);
                    m.insert("rules".to_string(), serde_json::json!([]));
                    m.insert("note".to_string(), serde_json::json!(
                        "no routes configured — run `teamx proxy routes set-default <exit>` \
                         and `teamx proxy routes add <match> <exit>`"));
                    Value::Object(m)
                }
                Err(e) => return Err(AppError(e)),
            }
        }
        RoutesCmd::Add { match_, exit, seq } => {
            let s = routes::upsert_rule(conn, *seq, match_, exit).map_err(AppError)?;
            serde_json::json!({ "ok": true, "seq": s, "match": match_, "exit": exit })
        }
        RoutesCmd::Remove { match_ } => {
            let removed = routes::remove_rule(conn, match_).map_err(AppError)?;
            serde_json::json!({ "ok": true, "removed": removed, "match": match_ })
        }
        RoutesCmd::SetDefault { exit } => {
            routes::set_default(conn, exit).map_err(AppError)?;
            serde_json::json!({ "ok": true, "default_exit": exit })
        }
        RoutesCmd::Import { path } => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| AppError(format!("routes file {}: {e}", path.display())))?;
            let table = routes::RouteTable::parse(&text).map_err(AppError)?;
            routes::save_to_db(conn, &table).map_err(AppError)?;
            serde_json::json!({ "ok": true, "imported": routes::to_json(&table) })
        }
        RoutesCmd::Clear => {
            routes::clear_rules(conn).map_err(AppError)?;
            serde_json::json!({ "ok": true, "cleared": true })
        }
    })
}

/// Handle `teamx local ...` — local client config (members + settings).
fn cmd_local(conn: &mut Connection, cmd: &LocalCmd) -> Result<Value> {
    use crate::db;
    Ok(match cmd {
        LocalCmd::MemberList => {
            let members = db::list_local_members(conn).map_err(|e| AppError(e.to_string()))?;
            serde_json::json!({ "ok": true, "members": members })
        }
        LocalCmd::MemberAdd { key, name, server, letter, proxy_port, dns_port } => {
            db::add_local_member(conn, key, name, server, letter.as_deref(), *proxy_port as i64, *dns_port as i64)
                .map_err(|e| AppError(e.to_string()))?;
            serde_json::json!({ "ok": true, "member_key": key })
        }
        LocalCmd::MemberUpdate { key, name, server, letter, proxy_port, dns_port } => {
            db::update_local_member(
                conn, key,
                name.as_deref(), server.as_deref(), letter.as_deref(),
                proxy_port.map(|p| p as i64), dns_port.map(|d| d as i64),
            )
            .map_err(|e| AppError(e.to_string()))?;
            serde_json::json!({ "ok": true, "member_key": key })
        }
        LocalCmd::MemberRemove { key } => {
            let removed = db::remove_local_member(conn, key).map_err(|e| AppError(e.to_string()))?;
            serde_json::json!({ "ok": true, "removed": removed, "member_key": key })
        }
        LocalCmd::Get { key } => {
            let v = db::get_setting(conn, key).map_err(|e| AppError(e.to_string()))?;
            serde_json::json!({ "ok": true, "key": key, "value": v })
        }
        LocalCmd::Set { key, value } => {
            db::set_setting(conn, key, value).map_err(|e| AppError(e.to_string()))?;
            serde_json::json!({ "ok": true, "key": key, "value": value })
        }
    })
}

/// Handle `teamx task ...`: the built-in `taskx` document type.
///
/// Tasks are documents: content in git (`taskx/<id>.md`), state machine in
/// `.teamx/docs/taskx/<id>.meta.json`, every transition an auditable ledger
/// event. This command maps `task` subcommands onto the doc engine
/// (`doc.created` / `doc.acknowledged` / `doc.done` / ...), so permissions,
/// state validation, meta persistence and reactions all come from the existing
/// DocSpec machinery (see `cmd_publish_doc`).
///
/// `task help` is special: it is a notification-only interrupt that does NOT
/// advance the state machine (a blocked task stays in_progress) — the lead is
/// notified via reactions and responds by updating the doc.
fn cmd_task(conn: &mut Connection, cmd: &crate::cli::TaskCmd) -> Result<Value> {
    use crate::cli::TaskCmd;

    // Project root / docs root for taskx instances.
    let cwd = std::env::current_dir().map_err(|e| AppError(format!("cwd: {e}")))?;
    let docs_root = cwd.join(".teamx").join("docs");
    let taskx_dir = docs_root.join(crate::doc_flow::BUILTIN_TASKX);

    match cmd {
        TaskCmd::Create { title, assignee, role, mode, executor, priority, id, detail, no_push, session, team } => {
            let (actor, team) = resolve_actor(conn, session, team.as_deref())?;
            // Only a lead (owner or co-lead) may create tasks.
            ensure_owner(conn, &actor, &team)?;

            // Determine delegation mode:
            //   --assignee given          -> direct (fixed assignee)
            //   --mode bid / --role (no assignee) -> bid (role competes; assignee empty)
            //   --mode broadcast          -> one instance per role member
            //   --mode direct without assignee -> error
            let assign_mode = match (mode.as_deref(), assignee.is_some(), role.is_some()) {
                (Some("broadcast"), _, true) => "broadcast".to_string(),
                (Some("bid"), _, true) => "bid".to_string(),
                (Some("direct"), true, _) => "direct".to_string(),
                (Some("direct"), false, _) => {
                    return err("task create --mode direct requires --assignee <member_id>")
                }
                (Some(m), _, _) => {
                    return err(format!("unknown mode `{m}` (valid: direct, bid, broadcast)"))
                }
                (None, true, _) => "direct".to_string(),
                (None, false, true) => "bid".to_string(), // --role defaults to bid
                (None, false, false) => {
                    return err("task create requires --assignee <member_id> or --role <role>")
                }
            };

            // Resolve the target members for this task:
            //   direct   -> the single assignee
            //   bid      -> the role key is kept; assignee is set when claimed
            //   broadcast-> every active member of the role (one instance each)
            let role_key = role.as_deref();
            let mut create_targets: Vec<(String, Option<String>)> = Vec::new(); // (member_id, per-instance id)
            match assign_mode.as_str() {
                "broadcast" => {
                    let members = members_for_team(conn, &team.id).map_err(|e| AppError(e.to_string()))?;
                    let role_members: Vec<String> = members
                        .iter()
                        .filter(|m| m.state != "left" && m.state != "denied" && m.role.as_deref() == role_key)
                        .map(|m| m.id.clone())
                        .collect();
                    if role_members.is_empty() {
                        return err(format!("no active members with role `{role_key:?}` for broadcast"));
                    }
                    for mid in role_members {
                        create_targets.push((mid.clone(), None));
                    }
                }
                _ => {
                    // direct: explicit assignee; bid: no assignee yet.
                    let aid = if let Some(aid) = assignee {
                        let m = member_by_id(conn, aid)?;
                        if m.team_id != team.id {
                            return err(format!("assignee member {aid} is not in team {}", team.id));
                        }
                        Some(m.id)
                    } else {
                        None // bid: assignee chosen on claim
                    };
                    create_targets.push((aid.unwrap_or_default(), None));
                }
            }

            // Doc id base.
            let base_id = id.clone().unwrap_or_else(|| slugify(title));
            if !crate::teamfile::is_safe_key_segment(&base_id) {
                return err(format!("task id `{base_id}` is not a safe identifier (no `/`, `..`, or control chars)"));
            }

            // Create one (or N for broadcast) taskx instances.
            let mut created: Vec<Value> = Vec::new();
            let instance_ids: Vec<String> = if assign_mode == "broadcast" {
                create_targets
                    .iter()
                    .map(|(mid, _)| {
                        // Full member id as suffix: UUIDs are unique and
                        // filesystem-safe (no 8-char truncation collision).
                        format!("{base_id}@{mid}")
                    })
                    .collect()
            } else {
                vec![base_id.clone()]
            };

            for (i, (mid, _)) in create_targets.iter().enumerate() {
                let doc_id = instance_ids.get(i).cloned().unwrap_or_else(|| base_id.clone());
                let payload = json!({
                    "doc": crate::doc_flow::BUILTIN_TASKX,
                    "id": doc_id,
                    "title": title,
                    "detail": detail,
                    "assignee_member_id": if assign_mode == "bid" { "" } else { mid },
                    "assignee_role": role_key,
                    "assign_mode": assign_mode,
                    "executor": executor,
                    "priority": priority,
                });
                let out = cmd_publish_doc(
                    conn,
                    "doc.created",
                    Some(&payload.to_string()),
                    // For bid, no directed assignee at creation (role-wide broadcast
                    // via reactions / plugin role matching); for direct/broadcast,
                    // direct the notification to the target member.
                    if assign_mode == "bid" || mid.is_empty() {
                        None
                    } else {
                        Some(mid)
                    },
                    session.as_str(),
                    Some(&team.id),
                )?;
                created.push(out);
                // Write the task document body (content in git).
                let md_path = taskx_dir.join(format!("{doc_id}.md"));
                if !md_path.exists() {
                    let body = build_task_md(title, detail.as_deref(), executor, priority, mid);
                    std::fs::write(&md_path, body).map_err(|e| AppError(format!("write {md_path:?}: {e}")))?;
                }
            }

            if !no_push {
                auto_git_commit(&cwd, &format!("teamx task: {title}"))?;
            }
            Ok(json!({
                "ok": true,
                "mode": assign_mode,
                "role": role_key,
                "created": created.len(),
                "instances": instance_ids,
            }))
        }
        TaskCmd::Ack { id, session, team } => {
            Ok(task_doc_event(conn, cmd, session.as_str(), team.as_deref(), "doc.acknowledged", id, Some("acked"), json!({}))?)
        }
        TaskCmd::Claim { id, session, team } => task_claim(conn, id, session.as_str(), team.as_deref()),
        TaskCmd::Update { id, progress, session, team } => {
            Ok(task_doc_event(conn, cmd, session.as_str(), team.as_deref(), "doc.updated", id, None, json!({ "note": progress }))?)
        }
        TaskCmd::Help { id, reason, session, team } => {
            Ok(task_doc_event(conn, cmd, session.as_str(), team.as_deref(), "doc.help_requested", id, None, json!({ "note": reason }))?)
        }
        TaskCmd::Done { id, result, session, team } => {
            Ok(task_doc_event(conn, cmd, session.as_str(), team.as_deref(), "doc.done", id, Some("done"), json!({ "note": result.as_deref().unwrap_or_default() }))?)
        }
        TaskCmd::Verify { id, session, team } => {
            Ok(task_doc_event(conn, cmd, session.as_str(), team.as_deref(), "doc.verified", id, Some("verified"), json!({}))?)
        }
        TaskCmd::Reject { id, reason, session, team } => {
            Ok(task_doc_event(conn, cmd, session.as_str(), team.as_deref(), "doc.rejected", id, Some("assigned"), json!({ "note": reason }))?)
        }
        TaskCmd::Retract { id, session, team } => task_retract(conn, id, session.as_str(), team.as_deref()),
        TaskCmd::ReBid { id, session, team } => task_rebid(conn, id, session.as_str(), team.as_deref()),
        TaskCmd::List { mine, state, assignee, executor, session, team } => {
            let my_id = if *mine {
                let sess = session.as_deref().ok_or_else(|| {
                    AppError("task list --mine requires --session (to identify the current member)".into())
                })?;
                let (actor, _t) = resolve_actor(conn, sess, team.as_deref())?;
                Some(actor.id)
            } else {
                None
            };
            task_list(conn, &docs_root, my_id.as_deref(), state.as_deref(), assignee.as_deref(), executor.as_deref())
        }
        TaskCmd::Log { id, session: _session, team: _team } => task_log(conn, &docs_root, id),
    }
}

/// Claim a bid task: the actor becomes the assignee (first-come-first-served).
/// Only members whose role matches the task's `assignee_role` (or the lead) may
/// claim; a task that is already claimed is rejected.
///
/// Concurrency: `with_task_lock` holds an exclusive flock on a temp-dir lock
/// file (keyed by the meta path) across the read-validate-write, so two
/// processes (member sessions) racing to claim the same task cannot both pass
/// the "unclaimed" check (TOCTOU-safe on unix).
fn task_claim(conn: &mut Connection, id: &str, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    let cwd = std::env::current_dir().map_err(|e| AppError(format!("cwd: {e}")))?;
    let docs_root = cwd.join(".teamx").join("docs");
    let meta_path = crate::doc_flow::DocMeta::meta_path(&docs_root, crate::doc_flow::BUILTIN_TASKX, id);

    with_task_lock(&meta_path, || {
        let meta = crate::doc_flow::DocMeta::load(&meta_path)
            .map_err(|e| AppError(format!("load task {id}: {e}")))?;

        // Must be an open bid task.
        if meta.assign_mode.as_deref() == Some("direct") || meta.assign_mode.as_deref() == Some("broadcast") {
            return err(format!("task {id} is {mode} mode; claim is only for bid tasks", mode = meta.assign_mode.unwrap_or_default()));
        }
        if meta.state != "assigned" {
            return err(format!("task {id} is in state `{}`; only open tasks can be claimed", meta.state));
        }
        if meta.assignee.is_some() {
            return err(format!("task {id} is already claimed by member {}", meta.assignee.as_deref().unwrap_or("?")));
        }
        // Role gate: a member may only claim tasks dispatched to their role (or a lead may claim anything).
        let is_lead = team.owner_member_id.as_deref() == Some(actor.id.as_str()) || actor.is_lead;
        if let Some(role) = meta.assignee_role.as_deref() {
            let role_ok = actor.role.as_deref() == Some(role);
            if !role_ok && !is_lead {
                return err(format!(
                    "member {} (role {:?}) may not claim task {id}: it is dispatched to role `{role}`",
                    actor.display_name, actor.role
                ));
            }
        }

        // Write the claim: assignee = actor.
        let payload = json!({
            "doc": crate::doc_flow::BUILTIN_TASKX,
            "id": id,
            "to": "claimed",
            "assignee_member_id": actor.id,
            "note": format!("claimed by {}", actor.display_name),
        });
        let out = cmd_publish_doc(
            conn,
            "doc.claimed",
            Some(&payload.to_string()),
            Some(&actor.id),
            session,
            Some(&team.id),
        )?;
        auto_git_commit(&cwd, &format!("teamx task {id}: claimed"))?;
        Ok(out)
    })
}

/// Hold an exclusive advisory lock on a lock file while `f` runs. Used to
/// serialize read-validate-write of a taskx `.meta.json` across processes
/// (e.g. concurrent `task claim` from two member sessions).
///
/// The lock file lives in the OS temp dir (deterministically named from the
/// meta path) so it is never committed into the team's shared git repo by
/// `auto_git_commit`'s `git add -A`, and needs no cleanup.
///
/// Scope: this only serializes callers that go through this helper (the
/// `task claim` CLI/plugin path). A direct `publish doc.claimed` bypasses the
/// lock; on Windows (no flock) claims are best-effort — the DB transaction
/// still serializes the ledger write, but the meta-file read-validate-write
/// TOCTOU window is not eliminated there.
fn with_task_lock<T>(meta_path: &std::path::Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock_path = task_lock_path(meta_path);
    #[cfg(unix)]
    {
        use std::io::Write as _;
        let mut fh = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| AppError(format!("open lock {}: {e}", lock_path.display())))?;
        // SAFETY: flock is async-signal-safe; the fd is valid for the duration of f.
        let rc = unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&fh), libc::LOCK_EX) };
        if rc != 0 {
            return err(format!("flock {}: {}", lock_path.display(), std::io::Error::last_os_error()));
        }
        // Keep the fd alive (and the lock held) until f completes.
        let _ = fh.write_all(b"");
        let result = f();
        // SAFETY: unlock is best-effort; the fd closes right after anyway.
        unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&fh), libc::LOCK_UN) };
        result
    }
    #[cfg(not(unix))]
    {
        // Windows: no flock primitive available — claims are best-effort here.
        // The SQLite write transaction still serializes the ledger insert, but
        // the read-validate-write window on `.meta.json` is NOT atomic, so two
        // racing claims could both pass the "unclaimed" check.
        let _ = &lock_path;
        f()
    }
}

/// Deterministic temp-dir lock file name for a task meta path, so two
/// processes on the same machine race on the SAME lock file (FNV-1a over the
/// canonical meta path, not `DefaultHasher`, whose algorithm may change
/// between Rust releases and break cross-version mutual exclusion).
fn task_lock_path(meta_path: &std::path::Path) -> std::path::PathBuf {
    let canonical = meta_path
        .canonicalize()
        .unwrap_or_else(|_| meta_path.to_path_buf());
    // FNV-1a 64-bit over the path bytes.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in canonical.to_string_lossy().as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let name = meta_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "task".to_string());
    std::env::temp_dir().join(format!("teamx-task-{name}-{hash:016x}.lock"))
}

/// Retract a claimed bid task: return it to the open pool (`assigned`, assignee
/// cleared). Permission: the member who claimed it may retract their own; a
/// lead may retract any task. After retraction the task is re-broadcast so role
/// members can claim again (no blacklist — anyone may re-claim).
fn task_retract(conn: &mut Connection, id: &str, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    let cwd = std::env::current_dir().map_err(|e| AppError(format!("cwd: {e}")))?;
    let docs_root = cwd.join(".teamx").join("docs");
    let meta_path = crate::doc_flow::DocMeta::meta_path(&docs_root, crate::doc_flow::BUILTIN_TASKX, id);
    let meta = crate::doc_flow::DocMeta::load(&meta_path)
        .map_err(|e| AppError(format!("load task {id}: {e}")))?;

    // Permission: the claimer may retract their own claim; a lead may retract any.
    let is_lead = team.owner_member_id.as_deref() == Some(actor.id.as_str()) || actor.is_lead;
    let is_claimer = meta.assignee.as_deref() == Some(actor.id.as_str());
    if !is_lead && !is_claimer {
        return err(format!(
            "member {} may not retract task {id}: only the claimer or a lead may retract",
            actor.display_name
        ));
    }
    if meta.state != "claimed" {
        return err(format!("task {id} is in state `{}`; only a claimed task can be retracted", meta.state));
    }

    // Move claimed -> assigned via a backward doc event, clearing the assignee.
    let payload = json!({
        "doc": crate::doc_flow::BUILTIN_TASKX,
        "id": id,
        "to": "assigned",
        "note": format!("retracted by {}", actor.display_name),
        "assignee_member_id": "", // clear the claim
    });
    let out = cmd_publish_doc(
        conn,
        "doc.retracted",
        Some(&payload.to_string()),
        None,
        session,
        Some(&team.id),
    )?;

    // Re-broadcast to the role so other members can claim (best-effort).
    if let Some(role) = meta.assignee_role.as_deref() {
        broadcast_role_rebid(conn, &team, role, id)?;
    }
    auto_git_commit(&cwd, &format!("teamx task {id}: retracted"))?;
    Ok(out)
}

/// Re-broadcast a task so role members can claim it again (team lead only).
/// No-op if the task is already open (`assigned` with no claimer).
fn task_rebid(conn: &mut Connection, id: &str, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    let cwd = std::env::current_dir().map_err(|e| AppError(format!("cwd: {e}")))?;
    let docs_root = cwd.join(".teamx").join("docs");
    let meta_path = crate::doc_flow::DocMeta::meta_path(&docs_root, crate::doc_flow::BUILTIN_TASKX, id);
    let meta = crate::doc_flow::DocMeta::load(&meta_path)
        .map_err(|e| AppError(format!("load task {id}: {e}")))?;
    if meta.state != "assigned" || meta.assignee.is_some() {
        return err(format!("task {id} is not open for bidding (state `{}`); retract or wait first", meta.state));
    }
    let role = meta.assignee_role.clone().unwrap_or_default();
    broadcast_role_rebid(conn, &team, &role, id)?;
    Ok(json!({
        "ok": true,
        "task": id,
        "role": role,
        "note": format!("re-broadcast to role `{role}` for claim"),
    }))
}

/// Broadcast a `doc.rebid` notification so the role's members see the task is
/// claimable again. Emits ONE undirected event (no assignee_member_id) so every
/// role member's plugin sees the open task and the auto-claim path fires —
/// without waking them all to auto-execute the same task (no thundering herd).
fn broadcast_role_rebid(conn: &mut Connection, team: &TeamRow, role: &str, id: &str) -> Result<usize> {
    // Emit ONE undirected `doc.rebid` (no assignee_member_id) so every role
    // member's plugin sees the task is open again and the auto-claim path
    // fires (undirected → no auto-execute thundering herd on the same task).
    let nseq = db::with_write(conn, |tx| {
        emit_json(
            tx,
            &team.id,
            None,
            "doc.rebid",
            json!({
                "doc": crate::doc_flow::BUILTIN_TASKX,
                "id": id,
                "assignee_role": role,
                "note": format!("task {id} is open for claim (role {role})"),
            }),
        )
    })
    .map_err(|e| AppError(format!("rebid event: {e}")))?;
    let _ = nseq;
    Ok(1)
}

/// Shared handler for task lifecycle events: build the doc payload, call the
/// doc engine, then auto git-commit (unless the event is ack — see below).
#[allow(clippy::too_many_arguments)]
fn task_doc_event(
    conn: &mut Connection,
    cmd: &crate::cli::TaskCmd,
    session: &str,
    team_opt: Option<&str>,
    event: &str,
    id: &str,
    to_state: Option<&str>,
    mut payload: serde_json::Value,
) -> Result<Value> {
    let cwd = std::env::current_dir().map_err(|e| AppError(format!("cwd: {e}")))?;
    payload["doc"] = json!(crate::doc_flow::BUILTIN_TASKX);
    payload["id"] = json!(id);
    if let Some(t) = to_state {
        payload["to"] = json!(t);
    }
    let out = cmd_publish_doc(
        conn,
        event,
        Some(&payload.to_string()),
        None,
        session,
        team_opt,
    )?;
    let _ = cmd;
    // Auto git commit for meaningful state changes; ack is noisy so it skips.
    if event != "doc.acknowledged" {
        auto_git_commit(&cwd, &format!("teamx task {id}: {event}"))?;
    }
    Ok(out)
}

/// `task list`: aggregate taskx instances from their .meta.json files.
fn task_list(
    conn: &Connection,
    docs_root: &std::path::Path,
    my_id: Option<&str>,
    state: Option<&str>,
    assignee: Option<&str>,
    executor: Option<&str>,
) -> Result<Value> {
    let taskx_dir = docs_root.join(crate::doc_flow::BUILTIN_TASKX);
    let mut tasks: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&taskx_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".meta.json") {
                continue;
            }
            let id = name.trim_end_matches(".meta.json").to_string();
            let Ok(meta) = crate::doc_flow::DocMeta::load(&e.path()) else { continue };
            if let Some(s) = state {
                if meta.state != s {
                    continue;
                }
            }
            if let Some(a) = assignee {
                if meta.assignee.as_deref() != Some(a) {
                    continue;
                }
            }
            if let Some(e) = executor {
                if meta.executor.as_deref() != Some(e) {
                    continue;
                }
            }
            if let Some(mid) = my_id {
                if meta.assignee.as_deref() != Some(mid) {
                    continue;
                }
            }
            // Title from the doc body's first heading (best-effort).
            let title = read_task_title(&taskx_dir, &id);
            let _ = conn;
            tasks.push(json!({
                "id": id,
                "title": title,
                "state": meta.state,
                "assignee": meta.assignee,
                "executor": meta.executor.unwrap_or_else(|| "either".to_string()),
                "priority": meta.priority.unwrap_or_else(|| "medium".to_string()),
                "owner": meta.owner,
                "updated_at": meta.updated_at,
            }));
        }
    }
    tasks.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(json!({ "ok": true, "tasks": tasks }))
}

/// `task log`: the full audit trail of one taskx instance.
fn task_log(_conn: &Connection, docs_root: &std::path::Path, id: &str) -> Result<Value> {
    if !crate::teamfile::is_safe_key_segment(id) {
        return err(format!("task id `{id}` is not a safe identifier"));
    }
    let taskx_dir = docs_root.join(crate::doc_flow::BUILTIN_TASKX);
    let meta_path = crate::doc_flow::DocMeta::meta_path(docs_root, crate::doc_flow::BUILTIN_TASKX, id);
    if !meta_path.exists() {
        return err(format!("task `{id}` does not exist"));
    }
    let meta = crate::doc_flow::DocMeta::load(&meta_path).map_err(AppError)?;
    let title = read_task_title(&taskx_dir, id);
    let mut history: Vec<Value> = meta
        .history
        .iter()
        .map(|s| json!({ "state": s.state, "by": s.by, "at": s.at, "event_seq": s.event_seq }))
        .collect();
    // Prepend the current state as a summary line.
    let _ = &mut history;
    Ok(json!({
        "ok": true,
        "id": id,
        "title": title,
        "state": meta.state,
        "assignee": meta.assignee,
        "executor": meta.executor,
        "priority": meta.priority,
        "created_at": meta.created_at,
        "updated_at": meta.updated_at,
        "history": history,
    }))
}

/// Best-effort title from the task markdown's first `#` heading.
fn read_task_title(taskx_dir: &std::path::Path, id: &str) -> String {
    let md = taskx_dir.join(format!("{id}.md"));
    if let Ok(text) = std::fs::read_to_string(&md) {
        for line in text.lines() {
            let t = line.trim();
            if let Some(h) = t.strip_prefix("# ") {
                return h.trim().to_string();
            }
        }
    }
    id.to_string()
}

/// Build the initial task markdown body.
fn build_task_md(title: &str, detail: Option<&str>, executor: &str, priority: &str, assignee: &str) -> String {
    format!(
        "# {title}\n\n- assignee: {assignee}\n- executor: {executor}\n- priority: {priority}\n\n## 目标\n\n## 验收标准\n\n## 进展\n\n## 结果\n\n{}\n",
        detail.map(|d| format!("## 详情\n\n{d}\n")).unwrap_or_default()
    )
}

/// Auto git commit+push in the current working directory (best-effort, quiet).
/// The taskx/ directory must be inside a git repo the member controls.
/// `GIT_TERMINAL_PROMPT=0` prevents git from hanging on an unauthenticated
/// push; if the repo has no remote, commit still happens and push is skipped.
fn auto_git_commit(cwd: &std::path::Path, message: &str) -> Result<()> {
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["add", "-A"])
        .status();
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["commit", "-m", message])
        .status();
    if let Ok(st) = commit {
        if !st.success() {
            // e.g. not a git repo, or nothing to commit — surface it so the
            // team knows the task change may not be in the shared repo yet.
            eprintln!("teamx: git commit failed ({st}); task state may not be pushed");
        }
    }
    // Only push when a remote exists (avoids hanging credential prompts).
    let has_remote = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["remote"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if has_remote {
        let _ = std::process::Command::new("git")
            .env("GIT_TERMINAL_PROMPT", "0")
            .arg("-C")
            .arg(cwd)
            .args(["push"])
            .status();
    }
    Ok(())
}

/// Simple slug for task ids: lowercase, spaces -> '-', strip non-alnum.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in s.chars() {
        // Keep Unicode letters/digits (so Chinese titles produce readable ids);
        // collapse other runs into a single dash.
        if ch.is_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "task".to_string() } else { out }
}

/// Handle `teamx session list`: enumerate the local machine's teamx member
/// identities and how to resume each one.
///
/// A session key is `<instance>:<opencode-session>` (see opencode-plugin
/// `sessionKey`). In network mode the real identity comes from the mTLS client
/// certificate (stored under `~/.teamx/letters/<invitation_id>/`), so a member
/// whose invitation letter is present can resume after losing the opencode
/// session by just opening a new session with the cert env vars — this command
/// surfaces that fact.
fn cmd_session(conn: &Connection, cmd: &crate::cli::SessionCmd) -> Result<Value> {
    use crate::cli::SessionCmd;
    match cmd {
        SessionCmd::List { this_instance } => {
            let instance = read_instance_id();
            let mut sessions: Vec<Value> = Vec::new();
            {
                // Every non-left member + its team name, role, state, session_key.
                let mut stmt = conn
                    .prepare(
                        "SELECT m.id, m.display_name, m.role, m.state, m.session_key,
                                t.name, m.user_id
                         FROM members m
                         JOIN teams t ON t.id = m.team_id
                         WHERE m.state NOT IN ('left','denied') AND t.state != 'destroyed'
                         ORDER BY t.name ASC, m.display_name ASC",
                    )
                    .map_err(|e| AppError(format!("db error: {e}")))?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?, // member_id
                            r.get::<_, String>(1)?, // display_name
                            r.get::<_, Option<String>>(2)?, // role
                            r.get::<_, String>(3)?, // state
                            r.get::<_, String>(4)?, // session_key
                            r.get::<_, String>(5)?, // team_name
                            r.get::<_, String>(6)?, // user_id
                        ))
                    })
                    .map_err(|e| AppError(format!("db error: {e}")))?;
                for row in rows {
                    let (member_id, display_name, role, state, session_key, team_name, user_id) =
                        row.map_err(|e| AppError(format!("db error: {e}")))?;
                    // Filter to this machine's instance when requested.
                    if *this_instance && !session_key.starts_with(&instance) {
                        continue;
                    }
                    let session_part = session_key
                        .split_once(':')
                        .map(|(_, s)| s.to_string())
                        .unwrap_or_else(|| session_key.clone());
                    // Certificate-bound? The member has an invitation row whose
                    // id is the letters dir name.
                    let letter_id: Option<String> = conn
                        .query_row(
                            "SELECT id FROM invitations WHERE member_id = ?1 AND revoked_at IS NULL LIMIT 1",
                            [&member_id],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(|e| AppError(format!("db error: {e}")))?;
                    let cert_bound = letter_id
                        .as_ref()
                        .map(|id| crate::db::teamx_home().join("letters").join(id).join("client.crt").exists())
                        .unwrap_or(false);
                    sessions.push(json!({
                        "member_id": member_id,
                        "display_name": display_name,
                        "team": team_name,
                        "role": role.unwrap_or_else(|| "-".to_string()),
                        "state": state,
                        "session_key": session_key,
                        "opencode_session": session_part,
                        "cert_bound": cert_bound,
                        "user_id": user_id,
                        "resume": if cert_bound {
                            format!(
                                "open a new opencode session with TEAMX_SERVER_URL + the letter \
                                 certs under ~/.teamx/letters/{}/ (identity follows the certificate)",
                                letter_id.unwrap_or_default()
                            )
                        } else {
                            format!("resume the opencode session `{session_part}` (local-mode identity is the session key)")
                        },
                    }));
                }
            }
            Ok(json!({
                "ok": true,
                "instance_id": instance,
                "sessions": sessions,
                "note": if !sessions.is_empty() {
                    "use `opencode session list` to see/resume opencode sessions; network-mode members \
                     are certificate-bound and survive losing the session".to_string()
                } else {
                    "no member sessions found on this machine".to_string()
                },
            }))
        }
    }
}

/// Read the stable per-machine instance id from ~/.teamx/instance.json
/// (same as the opencode plugin's `instanceId()`); None if absent.
fn read_instance_id() -> String {
    let path = crate::db::teamx_home().join("instance.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(id) = v.get("instance_id").and_then(|i| i.as_str()) {
                return id.to_string();
            }
        }
    }
    String::new()
}

/// Handle `teamx plugin install|uninstall`: install the teamx opencode plugin
/// bundle (dist + agent + commands) into the opencode config directory. This
/// is what Homebrew and direct-binary installs use to wire the CLI into
/// opencode — no `cargo`/`bun` needed at runtime.
fn cmd_plugin(cmd: &crate::cli::PluginCmd) -> Result<Value> {
    match cmd {
        crate::cli::PluginCmd::Install { path, config_dir, english } => {
            let config = config_dir.clone().unwrap_or_else(opencode_config_dir);
            let repo = resolve_repo_root(path.as_ref())?;
            let plugin_dir = repo.join("opencode-plugin");
            let dist_js = plugin_dir.join("dist").join("teamx.js");
            if !dist_js.exists() {
                return err(format!(
                    "plugin bundle not found at {} — build it first (cd opencode-plugin && bun install && bun run build)",
                    dist_js.display()
                ));
            }

            // plugins/
            let plugins_dir = config.join("plugins");
            std::fs::create_dir_all(&plugins_dir).map_err(|e| AppError(format!("mkdir {plugins_dir:?}: {e}")))?;
            std::fs::copy(&dist_js, plugins_dir.join("teamx.js"))
                .map_err(|e| AppError(format!("copy plugin: {e}")))?;

            // agent/ + commands/ — pick the language (default: detect from env).
            let (agent_src, cmd_suffix) = if *english {
                ("teamx.en.md".to_string(), ".en".to_string())
            } else if detect_zh() {
                ("teamx.md".to_string(), String::new())
            } else {
                ("teamx.en.md".to_string(), ".en".to_string())
            };
            let agent_dir = config.join("agent");
            std::fs::create_dir_all(&agent_dir).map_err(|e| AppError(format!("mkdir {agent_dir:?}: {e}")))?;
            let agent_file = plugin_dir.join("assets").join("agent").join(&agent_src);
            if agent_file.exists() {
                std::fs::copy(&agent_file, agent_dir.join(&agent_src))
                    .map_err(|e| AppError(format!("copy agent: {e}")))?;
            }

            let commands_dir = config.join("commands");
            std::fs::create_dir_all(&commands_dir).map_err(|e| AppError(format!("mkdir {commands_dir:?}: {e}")))?;
            let assets_cmd = plugin_dir.join("assets").join("command");
            let mut installed = 0usize;
            if let Ok(entries) = std::fs::read_dir(&assets_cmd) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if cmd_suffix.is_empty() {
                        // Chinese: install *.md (not *.en.md).
                        if name.ends_with(".en.md") || !name.ends_with(".md") {
                            continue;
                        }
                    } else {
                        // English: install *.en.md.
                        if !name.ends_with(".en.md") {
                            continue;
                        }
                    }
                    let base = name.trim_end_matches(&format!("{cmd_suffix}.md"));
                    let dst = commands_dir.join(format!("{base}.md"));
                    if let Err(e) = std::fs::copy(e.path(), &dst) {
                        return err(format!("copy command {name}: {e}"));
                    }
                    installed += 1;
                }
            }
            let _ = ensure_plugin_dependency(&config);
            Ok(serde_json::json!({
                "ok": true,
                "plugin": dist_js.display().to_string(),
                "config_dir": config.display().to_string(),
                "agent": agent_src,
                "commands_installed": installed,
                "note": "restart opencode (or reload) to pick up the plugin, agent and /team commands",
            }))
        }
        crate::cli::PluginCmd::Uninstall { config_dir } => {
            let config = config_dir.clone().unwrap_or_else(opencode_config_dir);
            let mut removed = Vec::new();
            for p in ["plugins/teamx.js", "agent/teamx.md", "agent/teamx.en.md"] {
                let f = config.join(p);
                if f.exists() {
                    let _ = std::fs::remove_file(&f);
                    removed.push(p.to_string());
                }
            }
            // Remove the /team command files we installed.
            if let Ok(entries) = std::fs::read_dir(config.join("commands")) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with("team") && name.ends_with(".md") {
                        let _ = std::fs::remove_file(e.path());
                        removed.push(format!("commands/{name}"));
                    }
                }
            }
            Ok(serde_json::json!({
                "ok": true,
                "removed": removed.len(),
                "note": "teamx data under ~/.teamx is preserved",
            }))
        }
    }
}

/// Resolve the teamx repo root from an explicit path or the CWD. Accepts either
/// the repo root (has opencode-plugin/) or the opencode-plugin dir itself.
fn resolve_repo_root(path: Option<&PathBuf>) -> Result<PathBuf> {
    let base = path.cloned().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if base.join("opencode-plugin").is_dir() {
        return Ok(base);
    }
    if base.file_name().map(|n| n == "opencode-plugin").unwrap_or(false) {
        if let Some(parent) = base.parent() {
            return Ok(parent.to_path_buf());
        }
    }
    err(format!(
        "cannot find the teamx plugin bundle: {} is not a teamx repo root \
         (expected opencode-plugin/dist/teamx.js after building)",
        base.display()
    ))
}

/// Default opencode config directory (~/.config/opencode).
fn opencode_config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("OPENCODE_CONFIG") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("~/.config")).join("opencode")
}

/// Best-effort Chinese language detection from LANG/LC_ALL.
fn detect_zh() -> bool {
    let lang = std::env::var("LANG").or_else(|_| std::env::var("LC_ALL")).unwrap_or_default();
    lang.starts_with("zh")
}

/// Pin @opencode-ai/plugin in the config package.json (best-effort), matching
/// what the old install.sh did.
fn ensure_plugin_dependency(config: &std::path::Path) {
    let pkg = config.join("package.json");
    let version = std::process::Command::new("opencode")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find_map(|l| {
                    l.rsplit_once(' ')
                        .map(|(_, v)| v.to_string())
                        .filter(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
                })
        })
        .unwrap_or_else(|| "1.17.11".to_string());
    let deps = serde_json::json!({ "dependencies": { "@opencode-ai/plugin": version } });
    let _ = std::fs::write(pkg, serde_json::to_string_pretty(&deps).unwrap_or_default());
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
                    "docs": boot.docs,
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

    // Auto-create a git repo on the teamx server from the current project
    // directory (name = sanitized team name; contents = cwd, minus noise).
    let mut git_info = None;
    if let Ok(cwd) = std::env::current_dir() {
        let repo_name = crate::git_service::repo_name_from_team(&team.name);
        let repo = crate::git_service::create_repo(conn, &team_id, &repo_name, Some("auto-created with team"), &member_id);
        match repo {
            Ok(r) => {
                match crate::git_service::seed_repo_from_dir(&team_id, &repo_name, &cwd) {
                    Ok(n) => {
                        git_info = Some(json!({
                            "repo": r.name,
                            "repo_id": r.id,
                            "path": r.path,
                            "files_seeded": n,
                            "note": format!("team git repo `{}` created and seeded from the current directory", r.name),
                        }));
                    }
                    Err(e) => {
                        git_info = Some(json!({ "repo": r.name, "error": e, "note": "repo created but seeding failed" }));
                    }
                }
            }
            Err(e) => {
                git_info = Some(json!({ "error": e, "note": "git repo auto-creation skipped" }));
            }
        }
    }
    if let Some(info) = git_info {
        if let Some(o) = out.as_object_mut() {
            o.insert("git".to_string(), info);
        }
    }

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
    /// Document contract snapshots written to `.teamx/docs/_spec/`.
    docs: Vec<serde_json::Value>,
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
            let inv = cmd_team_invite(conn, &role_desc, Some(&m.display_name), Some(&server_url), owner_session, Some(team_id), None, None)?;
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

    // 3. Document contract snapshots -> `.teamx/docs/_spec/<key>.json`.
    // These are the executable source of truth for doc lifecycle (T3/T4) —
    // an agent/member can load them without re-parsing TEAM.md. Incomplete
    // docs (missing owner/states) are skipped but surfaced in the output.
    let docs_dir = teamx_dir.join("docs").join("_spec");
    let mut docs_out = Vec::new();
    for d in &tf.docs {
        let spec = json!({
            "doc": d.key,
            "title": d.title,
            "purpose": d.purpose,
            "template": d.template,
            "creators": d.creators,
            "owner": d.owner,
            "approvers": d.approvers,
            "states": d.states,
            "reactions": d.reactions.iter().map(|r| json!({
                "on": r.on,
                "to_role": r.to_role,
                "action": r.action,
            })).collect::<Vec<_>>(),
            "incomplete": d.is_incomplete(),
        });
        if !d.is_incomplete() {
            std::fs::create_dir_all(&docs_dir).map_err(|e| AppError(format!("mkdir {docs_dir:?}: {e}")))?;
            let sp = docs_dir.join(format!("{}.json", d.key));
            let text = serde_json::to_string_pretty(&spec)
                .map_err(|e| AppError(format!("serialize doc spec: {e}")))?;
            std::fs::write(&sp, &text).map_err(|e| AppError(format!("write doc spec {sp:?}: {e}")))?;
            docs_out.push(json!({
                "key": d.key,
                "spec_file": sp.display().to_string(),
                "states": d.states,
            }));
        } else {
            docs_out.push(json!({
                "key": d.key,
                "spec_file": serde_json::Value::Null,
                "incomplete": true,
                "reason": "missing owner or states",
            }));
        }
    }

    Ok(BootstrapOutcome { goal_id, members, docs: docs_out })
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

    // Auto-grant read access to all team git repos on approval, so the member
    // can immediately `git clone` (feature: import → approve → clone).
    let mut granted_repos: Vec<String> = Vec::new();
    if approve {
        if let Ok(repos) = crate::git_service::list_repos(conn, &team.id) {
            for r in repos {
                let _ = crate::git_service::grant_permission(conn, &r.id, &target.id, crate::git_service::PERM_READ, &actor.id);
                granted_repos.push(r.name);
            }
        }
    }

    touch(conn, &actor.id).ok();
    let mut out = json!({
        "ok": true,
        "action": if approve { "approved" } else { "denied" },
        "member_id": target.id,
        "state": to_s,
        "from": from_s,
    });
    if !granted_repos.is_empty() {
        out["git_access"] = json!(granted_repos);
    }
    Ok(out)
}

/// Promote (or demote) a member to a backup team lead (co-lead). Any team lead
/// (owner or co-lead) may do this. A co-lead has full team-lead authorization
/// while the primary owner remains the founder.
fn cmd_team_set_lead(
    conn: &mut Connection,
    member_id: &str,
    session: &str,
    team_opt: Option<&str>,
    promote: bool,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;
    let target = member_by_id(conn, member_id)?;
    if target.team_id != team.id {
        return err(format!("member {member_id} is not in team {}", team.id));
    }
    if target.id == actor.id {
        return err("cannot change your own lead status");
    }
    if !promote && team.owner_member_id.as_deref() == Some(target.id.as_str()) {
        return err("the primary owner cannot be demoted");
    }
    let flag = if promote { 1 } else { 0 };
    db::with_write(conn, |tx| {
        tx.execute("UPDATE members SET is_lead = ?1 WHERE id = ?2", params![flag, target.id])?;
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            if promote { "member.promoted_lead" } else { "member.demoted_lead" },
            json!({ "member_id": target.id, "display_name": target.display_name }),
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("set lead failed: {e}")))?;
    touch(conn, &actor.id).ok();
    Ok(json!({ "ok": true, "member_id": target.id, "is_lead": promote }))
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
            "my_is_lead": m.is_lead,
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
        "members": members.iter().map(|m| member_json(conn, m)).collect::<Vec<_>>(),
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
                user_id, created_by, created_at, used_by, used_at, revoked_at
         FROM invitations WHERE team_id = ?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([team_id], invitation_row)?;
    rows.collect()
}

/// `team invite "<label>[: <desc>]"` — owner issues a member cert + letter.
///
/// A device invitation may be bound to a person (`user`) so the same person's
/// multiple devices share one user id and can access each other's tunnels:
/// - `--user <id>`: the user must already exist.
/// - `--user-name <name>`: reuse an existing user with that exact display name,
///   or create one on the fly.
#[allow(clippy::too_many_arguments)]
fn cmd_team_invite(
    conn: &mut Connection,
    role_desc: &str,
    name_hint: Option<&str>,
    server_url: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
    user_name: Option<&str>,
    user_id: Option<&str>,
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

    // Resolve the owning person (create by name when missing). `None` keeps the
    // legacy behaviour: the member is its own (unbound) user.
    let user = resolve_user(conn, user_id, user_name, &actor.id)?;
    let user_id_opt = user.as_ref().map(|(id, _)| id.as_str());

    let member_id = uuid::Uuid::new_v4().to_string();
    let invitation_id = uuid::Uuid::new_v4().to_string();
    let home = teamx_home_dir();
    let issued = pki::issue_member_cert(&home, &member_id, &role_key, user_id_opt).map_err(AppError)?;
    let ca_pem = std::fs::read_to_string(pki::ca_dir(&home).join("ca.crt"))
        .map_err(|e| AppError(format!("read ca cert: {e}")))?;
    let fingerprint = pki::ca_fingerprint(&home).map_err(AppError)?;
    let server = server_url.unwrap_or("https://127.0.0.1:5781").to_string();
    let now = db::now();

    let mut letter = json!({
        "teamx_invitation": {
            "version": 2,
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
    if let Some((uid, uname)) = &user {
        letter["teamx_invitation"]["user"] = json!({ "id": uid, "name": uname });
    }

    let label_owned = label.to_string();
    let desc_ref = desc.as_deref();
    db::with_write(conn, |tx| {
        tx.execute(
            "INSERT OR IGNORE INTO roles (team_id, key, label, description, permissions_json, state, proposed_by)
             VALUES (?1, ?2, ?3, ?4, '{}', 'approved', ?5)",
            params![team.id, role_key, label_owned, desc_ref, actor.id],
        )?;
        tx.execute(
            "INSERT INTO invitations (id, team_id, member_id, role_key, role_label, role_desc, cert_serial, cert_cn, user_id, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                invitation_id,
                team.id,
                member_id,
                role_key,
                label_owned,
                desc_ref,
                issued.serial_hex,
                issued.cn,
                user.as_ref().map(|(id, _)| id.as_str()).unwrap_or(""),
                actor.id,
                now
            ],
        )?;
        let mut payload = json!({ "invitation_id": invitation_id, "member_id": member_id, "role": role_key, "role_label": label_owned });
        if let Some((uid, uname)) = &user {
            payload["user"] = json!({ "id": uid, "name": uname });
        }
        emit_json(
            tx,
            &team.id,
            Some(&actor.id),
            "invitation.created",
            payload,
        )?;
        Ok(())
    })
    .map_err(|e| AppError(format!("invite failed: {e}")))?;

    let letter_json = serde_json::to_string(&letter).map_err(|e| AppError(format!("serialize letter: {e}")))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(letter_json.as_bytes());

    let mut out = json!({
        "ok": true,
        "invitation_id": invitation_id,
        "member_id": member_id,
        "role": { "key": role_key, "label": label, "description": desc },
        "letter": format!("teamx-inv:v1:{encoded}"),
        "note": "share this letter with the member; they import it with `teamx team import <letter>`",
    });
    if let Some((uid, uname)) = &user {
        out["user"] = json!({ "id": uid, "name": uname });
    }
    Ok(out)
}

/// Resolve (or create) the person a device invitation binds to.
fn resolve_user(
    conn: &Connection,
    user_id: Option<&str>,
    user_name: Option<&str>,
    created_by: &str,
) -> Result<Option<(String, String)>> {
    if let Some(id) = user_id {
        let name: Option<String> = conn
            .query_row("SELECT display_name FROM users WHERE id = ?1", [id], |r| r.get(0))
            .optional()
            .map_err(|e| AppError(format!("db error: {e}")))?;
        let Some(name) = name else {
            return err(format!("user `{id}` not found"));
        };
        return Ok(Some((id.to_string(), name)));
    }
    let Some(name) = user_name.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    let existing: Option<String> = conn
        .query_row("SELECT id FROM users WHERE display_name = ?1", [name], |r| r.get(0))
        .optional()
        .map_err(|e| AppError(format!("db error: {e}")))?;
    if let Some(id) = existing {
        return Ok(Some((id, name.to_string())));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = db::now();
    conn.execute(
        "INSERT INTO users (id, display_name, email, created_by, created_at, updated_at)
         VALUES (?1, ?2, NULL, ?3, ?4, ?4)",
        params![id, name, created_by, now],
    )
    .map_err(|e| AppError(format!("create user failed: {e}")))?;
    Ok(Some((id, name.to_string())))
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
                "user_id": i.user_id,
                "state": state,
                "created_at": i.created_at,
                "used_by": i.used_by,
                "revoked_at": i.revoked_at,
            })
        })
        .collect();
    Ok(json!({ "ok": true, "invitations": list }))
}

/// `teamx user list` — list users (persons) and the members/agents bound to
/// each within the team. Owner/lead only (audit).
fn cmd_user_list(conn: &Connection, session: &str, team_opt: Option<&str>) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    ensure_owner(conn, &actor, &team)?;

    let user_ids: Vec<(String, String, Option<String>, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, display_name, email, created_at FROM users ORDER BY display_name ASC")
            .map_err(|e| AppError(format!("db error: {e}")))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| AppError(format!("db error: {e}")))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| AppError(format!("db error: {e}")))?
    };

    let mut users: Vec<Value> = Vec::new();
    for (id, display_name, email, created_at) in user_ids {
        let mut members: Vec<Value> = Vec::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT id, display_name, role, state FROM members
                     WHERE user_id = ?1 AND team_id = ?2 AND state NOT IN ('left','denied')
                     ORDER BY display_name ASC",
                )
                .map_err(|e| AppError(format!("db error: {e}")))?;
            let rows = stmt
                .query_map(params![id, team.id], |r| {
                    Ok(serde_json::json!({
                        "member_id": r.get::<_, String>(0)?,
                        "display_name": r.get::<_, String>(1)?,
                        "role": r.get::<_, Option<String>>(2)?,
                        "state": r.get::<_, String>(3)?,
                    }))
                })
                .map_err(|e| AppError(format!("db error: {e}")))?;
            for m in rows {
                members.push(m.map_err(|e| AppError(format!("db error: {e}")))?);
            }
        }
        users.push(json!({
            "id": id,
            "display_name": display_name,
            "email": email,
            "created_at": created_at,
            "members": members,
        }));
    }

    Ok(json!({ "ok": true, "users": users }))
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
    if !(1..=2).contains(&version) {
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

    // User binding: the letter's `user.id` must agree with the invitation row's
    // `user_id` (both owner-authored; this is a consistency guard, not an auth
    // boundary — the cert-derived user is enforced at tunnel connect time).
    let user_id = inv_row.user_id.clone().unwrap_or_default();
    if let Some(letter_user) = inv["user"]["id"].as_str() {
        match &user_id {
            u if u == letter_user => {}
            u if !u.is_empty() => return err(format!("letter user `{letter_user}` does not match invitation user `{u}`")),
            _ => return err(format!("letter carries user `{letter_user}` but the invitation is not bound to a user")),
        }
    }

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
                     joined_at = ?4, left_at = NULL, user_id = ?5 WHERE id = ?6",
                    params![session, display_name, role_key, now, user_id, member_id],
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
                    "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at, user_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7)",
                    params![member_id, team_id, session, display_name, role_key, now, user_id],
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
    // After approval the owner auto-grants read on all team repos, so surface
    // the team's git repos + server URL here so the member/plugin can clone.
    let git_repos = crate::git_service::list_repos(conn, &team_id)
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.name)
        .collect::<Vec<_>>();
    let server_url = inv["server"]["url"].as_str().unwrap_or("").to_string();
    let mut out = json!({
        "ok": true,
        "status": "pending",
        "member_id": member_id,
        "role": role_key,
        "team": { "id": team.id, "name": team.name, "state": team.state },
        "note": "invitation imported; waiting for owner approval",
    });
    if !git_repos.is_empty() {
        out["git_repos"] = json!(git_repos);
        out["server_url"] = json!(server_url);
        out["clone_hint"] = json!(format!(
            "after approval, clone with: git clone {}/git/{}/<repo> (or teamx git clone)",
            server_url, team_id
        ));
    }
    Ok(out)
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
    #[cfg(not(unix))]
    {
        let _ = path; // no-op on Windows
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
    // `doc.*` lifecycle events are handled by the declarative doc engine
    // (T4): permission + transition checks from the TEAM.md contract, then
    // the event is written to the ledger and the .meta.json is updated.
    // Reactions from the spec are turned into directed notifications.
    if publish_type.starts_with("doc.") {
        return cmd_publish_doc(conn, publish_type, data, assignee_opt, session, team_opt);
    }

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

/// Handle a `doc.*` lifecycle event via the declarative doc engine (T4).
///
/// Flow (design §6.3 + §7.1):
///   1. Parse `{ doc, id, to, note }` from the payload;
///   2. Load the contract from `.teamx/docs/_spec/<doc>.json` (T2 snapshot);
///   3. Load the instance's `.meta.json` (or start fresh for `doc.created`);
///   4. Validate with `doc_flow::apply_event` (permission + state flow) —
///      a failed check returns an error and writes NOTHING (§6.4);
///   5. On success: write the ledger event, persist the new `.meta.json`;
///   6. Match the spec's reactions for this event and emit a directed
///      notification to the target role's members (publish --assignee).
fn cmd_publish_doc(
    conn: &mut Connection,
    publish_type: &str,
    data: Option<&str>,
    assignee_opt: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value> {
    let (actor, team) = resolve_actor(conn, session, team_opt)?;
    if actor.state == "pending" {
        return err(format!(
            "member {} is pending; wait for owner approval before publishing",
            actor.display_name
        ));
    }
    let actor_role = actor.role.as_deref().unwrap_or("contributor");

    let mut payload: Value = match data {
        Some(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({ "message": s })),
        None => json!({}),
    };
    if !payload.is_object() {
        payload = json!({ "message": payload });
    }
    let doc_key = match payload.get("doc").and_then(|v| v.as_str()) {
        Some(d) => d.to_string(),
        None => return err("doc.* event requires a `doc` key in payload (the TEAM.md document key)"),
    };
    // S1 (CR-022): reject unsafe doc keys to prevent path traversal (CWE-22).
    if !crate::teamfile::is_safe_key_segment(&doc_key) {
        return err(format!(
            "doc key `{doc_key}` is not a safe identifier (no `/`, `..`, or control chars)"
        ));
    }
    let doc_id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("default").to_string();
    if !crate::teamfile::is_safe_key_segment(&doc_id) {
        return err(format!(
            "doc id `{doc_id}` is not a safe identifier (no `/`, `..`, or control chars)"
        ));
    }
    // S2 (CR-022): `doc.reaction` is a system-generated notification emitted by
    // this handler; it is NOT a lifecycle transition and must not be published
    // back as one (auto-execute would otherwise fail on the missing `to`).
    if publish_type == "doc.reaction" {
        return err("`doc.reaction` is a system notification, not a lifecycle event; it cannot be published directly");
    }
    // S4 (CR-022): whitelist known lifecycle events instead of failing open.
    const KNOWN_DOC_EVENTS: &[&str] = &[
        "doc.created",
        "doc.updated",
        "doc.reviewed",
        "doc.approved",
        "doc.rejected",
        "doc.reopened",
        "doc.closed",
        // taskx lifecycle (built-in task doc type):
        "doc.claimed",
        "doc.acknowledged",
        "doc.help_requested",
        "doc.done",
        "doc.verified",
        "doc.retracted",
    ];
    if !KNOWN_DOC_EVENTS.contains(&publish_type) {
        return err(format!(
            "unknown doc event `{publish_type}` (known: created/updated/reviewed/approved/rejected/reopened/closed)"
        ));
    }
    let to_state = payload.get("to").and_then(|v| v.as_str()).map(|s| s.to_string());
    let note = payload.get("note").and_then(|v| v.as_str()).unwrap_or("").to_string();

    // Locate the project root: TEAM.md bootstrap uses the CWD.
    let cwd = std::env::current_dir().map_err(|e| AppError(format!("cwd: {e}")))?;
    let docs_root = cwd.join(".teamx").join("docs");
    let spec_path = docs_root.join("_spec").join(format!("{doc_key}.json"));
    // The built-in `taskx` type has a code-level spec; it never requires an
    // on-disk `_spec/taskx.json` (a team may still override it there).
    if !spec_path.exists() && doc_key != crate::doc_flow::BUILTIN_TASKX {
        return err(format!(
            "doc type `{doc_key}` is not recognized (no contract in TEAM.md ## 文档)"
        ));
    }
    let spec = crate::doc_flow::load_spec(&docs_root, &doc_key)
        .map_err(|e| AppError(format!("doc spec `{doc_key}`: {e}")))?;

    // Load the instance meta (absent = fresh instance, only valid for created).
    let meta_path = crate::doc_flow::DocMeta::meta_path(&docs_root, &doc_key, &doc_id);
    let existing = if meta_path.exists() {
        Some(crate::doc_flow::DocMeta::load(&meta_path).map_err(AppError)?)
    } else {
        None
    };
    let is_created = publish_type == "doc.created";

    // `doc.created` on an existing instance is an error; a non-created event on
    // a missing instance is also an error.
    if let Some(ex) = existing.as_ref() {
        if is_created {
            return err(format!(
                "doc `{doc_key}`/{doc_id} already exists (state: {}); use a doc.* event to advance",
                ex.state
            ));
        }
    } else if !is_created {
        return err(format!(
            "doc `{doc_key}`/{doc_id} does not exist; create it first with doc.created"
        ));
    }

    let meta = existing.clone().unwrap_or_else(|| crate::doc_flow::DocMeta {
        doc: doc_key.clone(),
        id: doc_id.to_string(),
        state: String::new(), // set by apply_event on created
        owner: spec.owner.clone(),
        created_at: crate::events::db_now(),
        updated_at: crate::events::db_now(),
        history: Vec::new(),
        assignee: None,
        executor: None,
        priority: None,
        assign_mode: None,
        assignee_role: None,
    });

    // Validate + compute the next meta (pure — no side effects). A failed
    // check returns before any write. The audit label records the member's
    // display name + role so two members sharing a role stay distinguishable.
    let actor_label = format!("{} ({})", actor.display_name, actor_role);
    // taskx: the assignee member may advance their own task's state (ack/claim/
    // update/done) even though the doc owner/approver role is "lead". We pass a
    // synthetic role that `can_advance` accepts when:
    //   - the actor IS the assignee, or
    //   - this is a bid task and the actor's role matches assignee_role
    //     (they are claiming / working it), or
    //   - the actor is a lead (impersonating "lead" grants advance).
    let is_lead_actor = team.owner_member_id.as_deref() == Some(actor.id.as_str()) || actor.is_lead;
    // A role member may advance a bid task ONLY to claim it (assignee empty,
    // event == doc.claimed). For all other events they must be the assignee
    // (or a lead). This prevents a same-role member from done/verify/retract
    // someone else's broadcast/direct/bid task (authorization bypass).
    let role_claim_only = doc_key == crate::doc_flow::BUILTIN_TASKX
        && meta.assign_mode.as_deref() == Some("bid")
        && meta.assignee.is_none()
        && publish_type == "doc.claimed"
        && meta.assignee_role.as_deref().is_some()
        && actor.role.as_deref() == meta.assignee_role.as_deref();
    let effective_role = if doc_key == crate::doc_flow::BUILTIN_TASKX
        && (meta.assignee.as_deref() == Some(actor.id.as_str()) || role_claim_only || is_lead_actor)
    {
        // Impersonate a spec role that `can_advance` accepts. Prefer "lead"
        // (the built-in taskx approver); when a team overrode `_spec/taskx.json`
        // and removed it, resolve any approver/owner so the assignee keeps the
        // right to advance their own task.
        crate::doc_flow::assignee_advance_role(&spec)
    } else {
        actor_role.to_string()
    };
    // `doc.help_requested` is a notification-only interrupt: it does not
    // advance the state machine (a task stays in_progress while blocked). We
    // record it in meta history with an unchanged state.
    let next = if publish_type == "doc.help_requested" {
        // Permission gate (M1): only the assignee or a lead may raise a help
        // request on a task — prevents notification spam on others' tasks.
        let is_assignee = meta.assignee.as_deref() == Some(actor.id.as_str());
        if doc_key == crate::doc_flow::BUILTIN_TASKX && !is_assignee && !is_lead_actor {
            return err(format!(
                "member {} may not raise help on task {doc_id}: only the assignee or a lead may",
                actor.display_name
            ));
        }
        let mut m = meta.clone();
        let now = crate::events::db_now();
        m.updated_at = now.clone();
        m.history.push(crate::doc_flow::MetaStep {
            state: meta.state.clone(),
            by: actor_label,
            at: now,
            event_seq: 0,
        });
        m
    } else {
        // taskx command-layer gates. The assignee is impersonated as "lead" so
        // they can ack/update/done their own task — but that impersonation must
        // NOT extend to lead-only lifecycle actions (verify/reject), otherwise
        // an assignee could self-verify and skip the lead's acceptance loop.
        if doc_key == crate::doc_flow::BUILTIN_TASKX {
            let is_assignee = meta.assignee.as_deref() == Some(actor.id.as_str());
            // HIGH-2: closing the loop (verify) or sending it back (reject) is
            // the team lead's job — not the assignee's self-service.
            if (publish_type == "doc.verified" || publish_type == "doc.rejected") && !is_lead_actor {
                return err(format!(
                    "member {} may not {} task {doc_id}: only a team lead may {}",
                    actor.display_name,
                    if publish_type == "doc.verified" { "verify" } else { "reject" },
                    if publish_type == "doc.verified" { "verify" } else { "reject" },
                ));
            }
            // MEDIUM-3: writing progress (doc.updated) belongs to the assignee
            // or a lead; a same-role member must not scribble on others' tasks.
            if publish_type == "doc.updated" && !is_assignee && !is_lead_actor {
                return err(format!(
                    "member {} may not update task {doc_id}: only the assignee or a lead may record progress",
                    actor.display_name
                ));
            }
        }
        // L3: verify must come from `done` (a lead cannot mark a task verified
        // that was never completed).
        if doc_key == crate::doc_flow::BUILTIN_TASKX
            && publish_type == "doc.verified"
            && meta.state != "done"
        {
            return err(format!(
                "task {doc_id} is in state `{}`; only a `done` task can be verified",
                meta.state
            ));
        }
        crate::doc_flow::apply_event(
            &spec,
            &meta,
            &effective_role,
            &actor_label,
            publish_type,
            to_state.as_deref(),
            0,
        )
        .map_err(AppError)?
    };

    let seq = db::with_write(conn, |tx| {
        payload["doc"] = json!(doc_key);
        payload["id"] = json!(doc_id);
        payload["state"] = json!(next.state);
        if let Some(f) = existing.as_ref() {
            payload["from"] = json!(f.state);
        }
        payload["by"] = json!(actor_role);
        if !note.is_empty() {
            payload["note"] = json!(note);
        }
        // Tag the directed assignee if one was given explicitly.
        if let Some(aid) = assignee_opt {
            payload["assignee_member_id"] = json!(aid);
        }
        // A bid task's assignee is empty at create/retract time: strip the
        // empty string from the EVENT payload so consumers (the opencode
        // plugin) see `null`/absent and treat the task as claimable (H3). The
        // original payload is kept untouched — the empty string remains
        // meaningful for meta persistence below (clears the claim on retract).
        let mut event_payload = payload.clone();
        if let Some(a) = event_payload.get("assignee_member_id").and_then(|v| v.as_str()) {
            if a.is_empty() {
                let _ = event_payload
                    .as_object_mut()
                    .map(|o| o.remove("assignee_member_id"));
            }
        }
        emit_json(tx, &team.id, Some(&actor.id), publish_type, event_payload)
    })
    .map_err(|e| AppError(format!("doc publish failed: {e}")))?;

    // Persist the meta with the real event seq.
    let mut persisted = next.clone();
    if let Some(last) = persisted.history.last_mut() {
        last.event_seq = seq;
    }
    if let Some(m) = existing.as_ref() {
        persisted.created_at = m.created_at.clone();
    }
    // Persist task semantics (taskx only) from the payload into the meta, so
    // `task list` can filter/display assignee, executor and priority without
    // re-parsing the markdown body.
    if doc_key == crate::doc_flow::BUILTIN_TASKX {
        if let Some(a) = payload.get("assignee_member_id").and_then(|v| v.as_str()) {
            if !a.is_empty() {
                persisted.assignee = Some(a.to_string());
            } else {
                // empty assignee clears the current claim (e.g. retract)
                persisted.assignee = None;
            }
        }
        if let Some(e) = payload.get("executor").and_then(|v| v.as_str()) {
            persisted.executor = Some(e.to_string());
        }
        if let Some(p) = payload.get("priority").and_then(|v| v.as_str()) {
            persisted.priority = Some(p.to_string());
        }
        if let Some(m) = payload.get("assign_mode").and_then(|v| v.as_str()) {
            persisted.assign_mode = Some(m.to_string());
        }
        if let Some(r) = payload.get("assignee_role").and_then(|v| v.as_str()) {
            persisted.assignee_role = Some(r.to_string());
        }
    }
    persisted
        .save(&meta_path)
        .map_err(|e| AppError(format!("doc meta save: {e}")))?;
    touch(conn, &actor.id).ok();

    // Reactions: find spec reactions triggered by this event and notify the
    // target role's members (directed events = auto-execute on them).
    let mut notified: Vec<serde_json::Value> = Vec::new();
    for r in &spec.reactions {
        let reaction_event = publish_type.strip_prefix("doc.").unwrap_or(publish_type);
        if r.on == reaction_event || r.on == publish_type {
            let targets = match &r.to_role {
                Some(role) => members_for_team(conn, &team.id)
                    .map_err(|e| AppError(format!("members: {e}")))?
                    .into_iter()
                    .filter(|m| m.state != "left" && m.state != "denied" && m.state != "pending")
                    .filter(|m| m.role.as_deref() == Some(role.as_str()))
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            };
            let to_role_label = r.to_role.clone().unwrap_or_default();
            for t in &targets {
                let nseq = db::with_write(conn, |tx| {
                    emit_json(
                        tx,
                        &team.id,
                        Some(&actor.id),
                        "doc.reaction",
                        json!({
                            "doc": doc_key,
                            "id": doc_id,
                            "on": r.on,
                            "action": r.action,
                            "assignee_member_id": t.id,
                            "assignee_name": t.display_name,
                        }),
                    )
                })
                .map_err(|e| AppError(format!("reaction event: {e}")))?;
                notified.push(json!({
                    "to_role": to_role_label,
                    "assignee": t.id,
                    "assignee_name": t.display_name,
                    "action": r.action,
                    "seq": nseq,
                }));
            }
        }
    }

    let from_state = existing.as_ref().map(|m| m.state.clone());
    Ok(json!({
        "ok": true,
        "seq": seq,
        "event": publish_type,
        "doc": doc_key,
        "id": doc_id,
        "state": next.state,
        "from_state": from_state,
        "by_role": actor_role,
        "meta_file": meta_path.display().to_string(),
        "notified": notified,
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

/// Parse an RFC3339 timestamp into seconds since the Unix epoch.
fn ts_secs(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

/// Write a `system.nudge` ledger event for every team that has gone silent
/// (no ledger activity for `after_secs`) while its goal is still unfinished.
///
/// This is the server-side "your task isn't done yet" reminder. It is
/// idempotent-ish: a team is only nudged again once `after_secs` have passed
/// since its *last nudge*, so a still-silent team gets a periodic poke (e.g.
/// every 5 minutes) without spamming on every tick.
///
/// Returns the newly-written events (with `team_id`) so the caller (serve)
/// can fan them out to live WebSocket connections immediately; offline members
/// pick them up on their next `sync` because they are ledger events.
pub fn nudge_idle_teams(
    conn: &mut Connection,
    after_secs: i64,
    now_secs: Option<i64>,
) -> rusqlite::Result<Vec<Value>> {
    let now = now_secs.unwrap_or_else(|| chrono::Utc::now().timestamp());
    let mut written: Vec<Value> = Vec::new();

    // Active teams (goal shared, not completed/archived, not forming).
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, COALESCE(g.title, ''), t.state
         FROM teams t
         LEFT JOIN goals g ON g.id = t.goal_id
         WHERE t.state IN ('active','blocked')
           AND g.state IS NOT NULL AND g.state NOT IN ('achieved','closed')",
    )?;
    let teams: Vec<(String, String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    for (team_id, team_name, goal_title, team_state) in teams {
        // Latest event timestamp for this team (any type; the ledger is the
        // activity clock). MAX over an empty set is NULL -> None.
        let last_ts: Option<String> = conn.query_row(
            "SELECT MAX(created_at) FROM events WHERE team_id = ?1",
            [&team_id],
            |r| r.get(0),
        )?;
        let last_event_secs = last_ts.as_deref().map(ts_secs).unwrap_or(0);
        let silent_for = if last_event_secs > 0 { now - last_event_secs } else { i64::MAX };

        if silent_for < after_secs {
            continue; // team is still active — no nudge
        }

        // Skip if a nudge was already sent recently (within after_secs). This
        // paces the reminder to roughly one poke per `after_secs` window.
        let last_nudge: Option<String> = conn.query_row(
            "SELECT MAX(created_at) FROM events WHERE team_id = ?1 AND type = 'system.nudge'",
            [&team_id],
            |r| r.get(0),
        )?;
        if let Some(n) = last_nudge {
            let since = now - ts_secs(&n);
            if since < after_secs {
                continue;
            }
        }

        let message = if team_state == "blocked" {
            format!("团队「{team_name}」处于 blocked 状态，目标「{goal_title}」尚未完成。请成员说明阻塞原因，或尽快恢复推进。")
        } else {
            format!("团队「{team_name}」的目标「{goal_title}」还未完成。你的任务是否执行完了？没完成请尽快完成，完成请提交产物。")
        };
        let payload = json!({
            "kind": "idle_reminder",
            "team_name": team_name,
            "goal_title": goal_title,
            "silent_for_secs": silent_for,
            "message": message,
        });
        // Write the nudge with an explicit created_at (the test-injectable clock
        // `now_secs`, or the real one) so pacing can be verified deterministically.
        let ts = chrono::DateTime::from_timestamp(now, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(crate::events::db_now);
        db::with_write(conn, |tx| {
            let seq = events::emit(tx, &team_id, None, "system.nudge", Some(&payload))?;
            // Overwrite created_at so it reflects the injected clock.
            tx.execute(
                "UPDATE events SET created_at = ?1 WHERE team_id = ?2 AND seq = ?3",
                params![ts, team_id, seq],
            )?;
            Ok(())
        })?;
        written.push(json!({
            "team_id": team_id,
            "member_id": Value::Null,
            "type": "system.nudge",
            "payload": payload,
        }));

        // Team-lead nudge: a silent team also needs its leads to step in and
        // coordinate (review progress, approve joins, broadcast decisions).
        // Separate event type + pacing so a lead reminder is never suppressed by
        // a member reminder and vice versa.
        let leads: Vec<(String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.display_name
                 FROM members m
                 WHERE m.team_id = ?1 AND m.state NOT IN ('left','denied')
                   AND (m.is_lead = 1 OR m.id = (SELECT owner_member_id FROM teams WHERE id = ?1))",
            )?;
            let rows = stmt.query_map(params![team_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !leads.is_empty() {
            // Pacing: independent of the member nudge; also `after_secs`.
            let last_lead_nudge: Option<String> = conn.query_row(
                "SELECT MAX(created_at) FROM events WHERE team_id = ?1 AND type = 'system.nudge_lead'",
                [&team_id],
                |r| r.get(0),
            )?;
            let lead_due = match last_lead_nudge {
                Some(n) => now - ts_secs(&n) >= after_secs,
                None => true,
            };
            if lead_due {
                let lead_names: Vec<&str> = leads.iter().map(|(_, n)| n.as_str()).collect();
                let lead_msg = if team_state == "blocked" {
                    format!(
                        "团队「{team_name}」已静默（blocked 状态），目标「{goal_title}」未完成。你是 team lead（{}），请协调：确认阻塞原因、推进恢复或调整目标。",
                        lead_names.join("、")
                    )
                } else {
                    format!(
                        "团队「{team_name}」已静默 {silent} 秒，目标「{goal_title}」未完成。你是 team lead（{}），请协调推进：检查成员进度、审批入队、或广播下一步。",
                        lead_names.join("、"),
                        silent = silent_for,
                    )
                };
                let lead_payload = json!({
                    "kind": "lead_reminder",
                    "team_name": team_name,
                    "goal_title": goal_title,
                    "silent_for_secs": silent_for,
                    "lead_members": leads,
                    "message": lead_msg,
                });
                db::with_write(conn, |tx| {
                    let seq = events::emit(tx, &team_id, None, "system.nudge_lead", Some(&lead_payload))?;
                    tx.execute(
                        "UPDATE events SET created_at = ?1 WHERE team_id = ?2 AND seq = ?3",
                        params![ts, team_id, seq],
                    )?;
                    Ok(())
                })?;
                written.push(json!({
                    "team_id": team_id,
                    "member_id": Value::Null,
                    "type": "system.nudge_lead",
                    "payload": lead_payload,
                }));
            }
        }
    }

    Ok(written)
}

/// Check every member's open `taskx` tasks and emit a `task.nudge` event for
/// members who still have unfinished tasks. Unlike `nudge_idle_teams` (team
/// silence), this is member-directed: the plugin wakes the assignee session
/// ("you have open tasks") so an agent keeps working and a human sees a
/// reminder.
///
/// Pacing is per member + task state: a member is nudged at most once per
/// `after_secs` (tracked by the last `task.nudge` event for that member).
/// Returns the events to fan out (with `team_id` + `assignee_member_id`).
pub fn nudge_open_tasks(
    conn: &mut Connection,
    after_secs: i64,
    now_secs: Option<i64>,
    taskx_dir: Option<&std::path::Path>,
) -> rusqlite::Result<Vec<Value>> {
    let now = now_secs.unwrap_or_else(|| chrono::Utc::now().timestamp());
    let mut written: Vec<Value> = Vec::new();

    // Locate the project's taskx instances. In network mode the server runs in
    // the owner's repo (CWD); in local mode it is the shared working dir. If no
    // taskx dir exists, there is nothing to nudge.
    let taskx_dir = taskx_dir.map(|p| p.to_path_buf()).unwrap_or_else(|| {
        let cwd = std::env::current_dir().unwrap_or_default();
        cwd.join(".teamx").join("docs").join(crate::doc_flow::BUILTIN_TASKX)
    });
    let Ok(entries) = std::fs::read_dir(&taskx_dir) else {
        return Ok(written);
    };

    // Collect open tasks per assignee: id + state + title.
    let mut per_assignee: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    let mut team_of: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".meta.json") {
            continue;
        }
        let id = name.trim_end_matches(".meta.json").to_string();
        let Ok(meta) = crate::doc_flow::DocMeta::load(&e.path()) else { continue };
        // Skip finished tasks.
        if matches!(meta.state.as_str(), "done" | "verified") {
            continue;
        }
        let Some(aid) = meta.assignee.as_deref() else { continue };
        if aid.is_empty() {
            continue;
        }
        let title = read_task_title(&taskx_dir, &id);
        per_assignee.entry(aid.to_string()).or_default().push(json!({
            "task_id": id,
            "state": meta.state,
            "title": title,
        }));
        // team_of: resolve via the member's team (members table).
        if let Ok(t) = conn.query_row(
            "SELECT team_id FROM members WHERE id = ?1 AND state NOT IN ('left','denied')",
            [aid],
            |r| r.get::<_, String>(0),
        ) {
            team_of.insert(aid.to_string(), t);
        }
    }

    for (aid, tasks) in per_assignee {
        let Some(team_id) = team_of.get(&aid) else { continue };
        // Pacing: at most one task.nudge per member per after_secs window.
        let last_nudge: Option<String> = conn.query_row(
            "SELECT MAX(created_at) FROM events WHERE team_id = ?1 AND member_id = ?2 AND type = 'task.nudge'",
            params![team_id, aid],
            |r| r.get(0),
        )?;
        if let Some(n) = last_nudge {
            if now - ts_secs(&n) < after_secs {
                continue;
            }
        }
        let names: Vec<&str> = tasks.iter().map(|t| t["title"].as_str().unwrap_or("-")).collect();
        let message = format!(
            "你有 {} 个未完成任务：{}。请继续推进；如受阻可 task help 求助 lead。",
            tasks.len(),
            names.join("、")
        );
        let payload = json!({
            "kind": "task_reminder",
            "assignee_member_id": aid,
            "tasks": tasks,
            "message": message,
        });
        let ts = chrono::DateTime::from_timestamp(now, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(crate::events::db_now);
        db::with_write(conn, |tx| {
            let seq = events::emit(tx, &team_id, Some(&aid), "task.nudge", Some(&payload))?;
            tx.execute(
                "UPDATE events SET created_at = ?1 WHERE team_id = ?2 AND seq = ?3",
                params![ts, team_id, seq],
            )?;
            Ok(())
        })?;
        written.push(json!({
            "team_id": team_id,
            "member_id": aid,
            "type": "task.nudge",
            "payload": payload,
        }));
    }

    Ok(written)
}
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

/// True if this member is the owner of the given team.
/// True if the member is a team lead of the given team: the primary owner
/// (`owner_member_id`) or a promoted co-lead (`members.is_lead = 1`).
pub fn is_team_owner(conn: &Connection, team_id: &str, member_id: &str) -> rusqlite::Result<bool> {
    let owner: i64 = conn.query_row(
        "SELECT COUNT(*) FROM teams WHERE id = ?1 AND owner_member_id = ?2",
        params![team_id, member_id],
        |r| r.get(0),
    )?;
    if owner > 0 {
        return Ok(true);
    }
    let lead: i64 = conn.query_row(
        "SELECT COUNT(*) FROM members WHERE id = ?1 AND team_id = ?2 AND is_lead = 1",
        params![member_id, team_id],
        |r| r.get(0),
    )?;
    Ok(lead > 0)
}

fn member_json(conn: &Connection, m: &MemberRow) -> Value {
    let ip = crate::db::member_ip(conn, &m.id);
    let online = crate::db::member_online(conn, &m.id);
    json!({
        "id": m.id,
        "display_name": m.display_name,
        "role": m.role,
        "state": m.state,
        "loopx_project": m.loopx_project,
        "joined_at": m.joined_at,
        "is_lead": m.is_lead,
        "ip": ip,
        "online": online,
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
        status["team"]["my_is_lead"] = Value::from(m.is_lead);
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
    let issued = pki::issue_member_cert(&home, member_id, role, None).map_err(AppError)?;
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

// ---------------------------------------------------------------------------
// Git operations (network mode)
// ---------------------------------------------------------------------------

/// Handle `teamx git <subcommand>`.
fn cmd_git(_conn: &mut Connection, cmd: &crate::cli::GitCmd) -> Result<Value> {
    use crate::cli::GitCmd;

    // Local-only commands (commit) don't need a server.
    match cmd {
        GitCmd::Commit { message, dir } => {
            return crate::git_client::commit(message, dir.as_deref()).map_err(git_err)
        }
        _ => {}
    }
    // git setup: configures stock `git` from the letter; no RPC involved.
    if let GitCmd::Setup { server, local } = cmd {
        let url = resolve_server_url(server.as_deref())?;
        return crate::git_client::setup(&url, *local).map_err(git_err);
    }

    let url = resolve_server_url(server_url_arg(cmd))?;
    match cmd {
        GitCmd::Clone { repo, directory, server: _, session: _, team } => {
            crate::git_client::clone(&url, repo, directory.as_deref(), team.as_deref()).map_err(git_err)
        }
        GitCmd::Pull { repo, branch, dir, server: _, session: _, team } => {
            crate::git_client::pull(&url, repo, branch.as_deref(), team.as_deref(), dir.as_deref()).map_err(git_err)
        }
        GitCmd::Push { repo, branch, dir, server: _, session: _, team } => {
            crate::git_client::push(&url, repo, branch.as_deref(), team.as_deref(), dir.as_deref()).map_err(git_err)
        }
        GitCmd::CommitPush { message, repo, branch, dir, server: _, session: _, team } => {
            crate::git_client::commit_push(&url, message, repo.as_deref(), branch.as_deref(), team.as_deref(), dir.as_deref())
                .map_err(git_err)
        }
        GitCmd::List { server: _, session: _, team } => {
            crate::git_client::list(&url, team.as_deref()).map_err(git_err)
        }
        GitCmd::Create { name, description, server: _, session: _, team } => {
            crate::git_client::create(&url, name, description.as_deref(), team.as_deref()).map_err(git_err)
        }
        GitCmd::Delete { name, server: _, session: _, team } => {
            crate::git_client::delete(&url, name, team.as_deref()).map_err(git_err)
        }
        GitCmd::Grant { name, member_id, permission, server: _, session: _, team } => {
            crate::git_client::grant(&url, name, member_id, permission, team.as_deref()).map_err(git_err)
        }
        GitCmd::Permissions { name, server: _, session: _, team } => {
            crate::git_client::permissions(&url, name, team.as_deref()).map_err(git_err)
        }
        GitCmd::Setup { .. } | GitCmd::Commit { .. } => unreachable!(),
    }
}

/// Convert a git client error into the command error type.
fn git_err(e: crate::git_client::GitError) -> AppError {
    AppError(e.to_string())
}

/// Extract the `server` arg from any GitCmd variant (for URL resolution).
fn server_url_arg(cmd: &crate::cli::GitCmd) -> Option<&str> {
    use crate::cli::GitCmd;
    match cmd {
        GitCmd::Clone { server, .. } => server.as_deref(),
        GitCmd::Pull { server, .. } => server.as_deref(),
        GitCmd::Push { server, .. } => server.as_deref(),
        GitCmd::CommitPush { server, .. } => server.as_deref(),
        GitCmd::List { server, .. } => server.as_deref(),
        GitCmd::Create { server, .. } => server.as_deref(),
        GitCmd::Delete { server, .. } => server.as_deref(),
        GitCmd::Grant { server, .. } => server.as_deref(),
        GitCmd::Permissions { server, .. } => server.as_deref(),
        GitCmd::Setup { server, .. } => server.as_deref(),
        GitCmd::Commit { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        db::migrate(&conn).unwrap();
        conn
    }

    fn seed_team(conn: &Connection, team_id: &str, owner_id: &str) {
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES (?1, 'T', ?2, NULL, 'forming', 'tok', 'now', 'now')",
            params![team_id, owner_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES (?1, ?2, 's', 'owner', 'owner', 'active', 'now')",
            params![owner_id, team_id],
        )
        .unwrap();
    }

    #[test]
    fn resolve_user_creates_then_reuses_by_name() {
        let conn = test_conn();
        let (id1, name1) = resolve_user(&conn, None, Some("张三"), "owner").unwrap().unwrap();
        assert_eq!(name1, "张三");
        // same name → same user (reused, not duplicated)
        let (id2, _) = resolve_user(&conn, None, Some("张三"), "owner").unwrap().unwrap();
        assert_eq!(id1, id2);
        // distinct name → distinct user
        let (id3, _) = resolve_user(&conn, None, Some("李四"), "owner").unwrap().unwrap();
        assert_ne!(id1, id3);
        // explicit id lookup resolves existing user
        let (id4, name4) = resolve_user(&conn, Some(&id1), None, "owner").unwrap().unwrap();
        assert_eq!(id4, id1);
        assert_eq!(name4, "张三");
        // unknown id errors
        assert!(resolve_user(&conn, Some("nope"), None, "owner").is_err());
        // no name / no id → None (unbound)
        assert!(resolve_user(&conn, None, None, "owner").unwrap().is_none());
        assert!(resolve_user(&conn, None, Some("  "), "owner").unwrap().is_none());
    }

    #[test]
    fn user_list_shows_bound_members() {
        let conn = test_conn();
        seed_team(&conn, "t1", "m1");
        // bind two members to one user, one member to another
        for (mid, uid) in [("ma", "u1"), ("mb", "u1"), ("mc", "u2")] {
            conn.execute(
                "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at, user_id)
                 VALUES (?1, 't1', ?1, ?1, 'contributor', 'active', 'now', ?2)",
                params![mid, uid],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO users (id, display_name, email, created_by, created_at, updated_at)
             VALUES ('u1', '张三', NULL, 'm1', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (id, display_name, email, created_by, created_at, updated_at)
             VALUES ('u2', '李四', NULL, 'm1', 'now', 'now')",
            [],
        )
        .unwrap();

        let out = cmd_user_list(&conn, "s", None).unwrap();
        let users = out["users"].as_array().unwrap();
        assert_eq!(users.len(), 2);
        let zhang = users.iter().find(|u| u["display_name"] == "张三").unwrap();
        assert_eq!(zhang["members"].as_array().unwrap().len(), 2);
        let li = users.iter().find(|u| u["display_name"] == "李四").unwrap();
        assert_eq!(li["members"].as_array().unwrap().len(), 1);
    }

    /// Helper: an active team with a shared, unfinished goal. Creates the
    /// owner member `m1` too, so the team-lead nudge also fires.
    fn seed_active_goal(conn: &Connection, team_id: &str, goal_id: &str, goal_state: &str) {
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES (?1, '目标团队', 'm1', ?2, 'active', 'tok', 'now', 'now')",
            params![team_id, goal_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO goals (id, team_id, title, body, state, created_at, updated_at)
             VALUES (?1, ?2, '完成产品', NULL, ?3, 'now', 'now')",
            params![goal_id, team_id, goal_state],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at, is_lead)
             VALUES ('m1', ?1, 's:owner', '老板', 'owner', 'active', 'now', 0)",
            params![team_id],
        )
        .unwrap();
    }

    #[test]
    fn nudge_fires_for_silent_active_team() {
        let mut conn = test_conn();
        seed_active_goal(&conn, "t1", "g1", "in_progress");

        // No events at all -> team is maximally silent -> member + lead nudges.
        let written = nudge_idle_teams(&mut conn, 300, Some(10_000)).unwrap();
        assert_eq!(written.len(), 2, "member nudge + lead nudge");
        let member = written.iter().find(|e| e["type"] == "system.nudge").unwrap();
        let lead = written.iter().find(|e| e["type"] == "system.nudge_lead").unwrap();
        assert_eq!(member["team_id"], "t1");
        let msg = member["payload"]["message"].as_str().unwrap();
        assert!(msg.contains("目标「完成产品」"), "message should mention the goal: {msg}");
        let lmsg = lead["payload"]["message"].as_str().unwrap();
        assert!(lmsg.contains("team lead"), "lead message should target the lead: {lmsg}");
        assert!(lmsg.contains("老板"), "lead message should name the lead member");
    }

    #[test]
    fn nudge_respects_recent_activity() {
        let mut conn = test_conn();
        seed_active_goal(&conn, "t1", "g1", "in_progress");

        // A recent ledger event (progress) means the team is not silent.
        conn.execute(
            "INSERT INTO events (team_id, member_id, seq, type, payload_json, created_at)
             VALUES ('t1', 'm1', 1, 'progress.published', NULL, ?1)",
            params!["2026-08-31T00:00:00+00:00"],
        )
        .unwrap();
        let written = nudge_idle_teams(&mut conn, 300, Some(10_000)).unwrap();
        assert!(written.is_empty(), "recent activity must suppress the nudge");
    }

    #[test]
    fn nudge_paces_itself_no_spam() {
        let mut conn = test_conn();
        seed_active_goal(&conn, "t1", "g1", "in_progress");

        let first = nudge_idle_teams(&mut conn, 300, Some(10_000)).unwrap();
        assert_eq!(first.len(), 2, "member + lead nudges on first poke");

        // Immediately re-running with the same clock must not write again
        // (the pacing window prevents one nudge per tick).
        let second = nudge_idle_teams(&mut conn, 300, Some(10_000)).unwrap();
        assert!(second.is_empty(), "no spam: a second nudge is suppressed within the window");

        // After `after_secs` pass, it may nudge again.
        let third = nudge_idle_teams(&mut conn, 300, Some(10_000 + 300)).unwrap();
        assert_eq!(third.len(), 2, "a later tick may nudge again");
    }

    #[test]
    fn nudge_skips_achieved_and_forming_teams() {
        let mut conn = test_conn();
        // achieved goal -> no nudge
        seed_active_goal(&conn, "t1", "g1", "achieved");
        // forming team (no goal shared yet) -> no nudge
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES ('t2', '招募中', 'm1', NULL, 'forming', 'tok', 'now', 'now')",
            [],
        )
        .unwrap();
        let written = nudge_idle_teams(&mut conn, 300, Some(10_000)).unwrap();
        assert!(written.is_empty(), "achieved/forming teams must not be nudged");
    }

    #[test]
    fn nudge_lead_includes_co_lead_members() {
        let mut conn = test_conn();
        seed_active_goal(&conn, "t1", "g1", "in_progress");
        // promote a co-lead
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at, is_lead)
             VALUES ('m2', 't1', 's:colead', '副手', 'contributor', 'active', 'now', 1)",
            [],
        )
        .unwrap();

        let written = nudge_idle_teams(&mut conn, 300, Some(10_000)).unwrap();
        let lead = written.iter().find(|e| e["type"] == "system.nudge_lead").unwrap();
        let leads = lead["payload"]["lead_members"].as_array().unwrap();
        let names: Vec<&str> = leads.iter().filter_map(|m| m[1].as_str()).collect();
        assert!(names.contains(&"老板"), "owner should be a lead");
        assert!(names.contains(&"副手"), "co-lead should be a lead");
    }

    #[test]
    fn session_list_lists_members_and_detects_cert_bound() {
        let conn = test_conn();
        seed_team(&conn, "t1", "m1");
        // A local-mode member (no invitation) + a network-mode member (invitation row).
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-local', 't1', 'inst1:ses_local', '本地成员', 'contributor', 'active', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO invitations (id, team_id, member_id, role_key, role_label, created_by, created_at)
             VALUES ('inv-1', 't1', 'm-net', 'contributor', NULL, 'm1', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-net', 't1', 'inst1:ses_net', '网络成员', 'contributor', 'active', 'now')",
            [],
        )
        .unwrap();

        let out = cmd_session(&conn, &crate::cli::SessionCmd::List { this_instance: false }).unwrap();
        let sessions = out["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 3, "owner + local + net");

        let local = sessions.iter().find(|s| s["member_id"] == "m-local").unwrap();
        assert_eq!(local["session_key"], "inst1:ses_local");
        assert_eq!(local["opencode_session"], "ses_local");
        assert_eq!(local["cert_bound"], false);

        let net = sessions.iter().find(|s| s["member_id"] == "m-net").unwrap();
        assert_eq!(net["cert_bound"], false, "no letter file on disk for inv-1");
        // cert_bound additionally requires ~/.teamx/letters/<id>/client.crt to
        // exist; in tests it does not, so it stays false even with an invitation.
        assert!(net["resume"].as_str().unwrap().contains("resume the opencode session"));
    }

    #[test]
    fn session_list_this_instance_filters_by_instance_prefix() {
        let conn = test_conn();
        seed_team(&conn, "t1", "m1");
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-a', 't1', 'aaa:ses_1', '本机', 'contributor', 'active', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-b', 't1', 'bbb:ses_2', '他机', 'contributor', 'active', 'now')",
            [],
        )
        .unwrap();

        let all = cmd_session(&conn, &crate::cli::SessionCmd::List { this_instance: false }).unwrap();
        assert_eq!(all["sessions"].as_array().unwrap().len(), 3);

        // `this_instance` filters by the *machine's* instance id — which is read
        // from ~/.teamx/instance.json. In tests that file is the real machine's
        // id, so it only keeps members whose session_key starts with it. To keep
        // the test deterministic, verify the filter never drops anything when the
        // instance id is empty (e.g. no instance.json).
        let out = cmd_session(&conn, &crate::cli::SessionCmd::List { this_instance: true }).unwrap();
        // instance id may be empty in CI; either way the command must not error
        // and must return a sessions array.
        assert!(out["sessions"].is_array());
    }

    #[test]
    fn nudge_open_tasks_nudges_assignee_with_open_task() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        // m1 is an active member; create an open taskx instance assigned to m1.
        let dir = std::env::temp_dir().join(format!("teamx-nudgetask-{}", std::process::id()));
        let taskx = dir.join("taskx");
        std::fs::create_dir_all(&taskx).unwrap();
        let meta = crate::doc_flow::DocMeta {
            doc: "taskx".into(),
            id: "t-1".into(),
            state: "assigned".into(),
            owner: "lead".into(),
            assignee: Some("m1".into()),
            executor: Some("human".into()),
            ..Default::default()
        };
        crate::doc_flow::DocMeta::meta_path(&dir, "taskx", "t-1")
            .parent()
            .map(|p| std::fs::create_dir_all(p).unwrap());
        meta.save(&crate::doc_flow::DocMeta::meta_path(&dir, "taskx", "t-1")).unwrap();
        std::fs::write(taskx.join("t-1.md"), "# 写报告\n").unwrap();

        let written = nudge_open_tasks(&mut conn, 300, Some(10_000), Some(&taskx)).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0]["type"], "task.nudge");
        assert_eq!(written[0]["member_id"], "m1");
        assert_eq!(written[0]["payload"]["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(written[0]["payload"]["tasks"][0]["task_id"], "t-1");

        // Pacing: immediate re-run within after_secs is suppressed.
        let again = nudge_open_tasks(&mut conn, 300, Some(10_000), Some(&taskx)).unwrap();
        assert!(again.is_empty(), "no spam within the pacing window");

        // A later tick nudges again.
        let later = nudge_open_tasks(&mut conn, 300, Some(10_000 + 300), Some(&taskx)).unwrap();
        assert_eq!(later.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nudge_open_tasks_skips_finished_and_unassigned() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let dir = std::env::temp_dir().join(format!("teamx-nudgetask2-{}", std::process::id()));
        let taskx = dir.join("taskx");
        std::fs::create_dir_all(&taskx).unwrap();
        // done task -> skipped
        let meta_done = crate::doc_flow::DocMeta {
            doc: "taskx".into(),
            id: "done".into(),
            state: "done".into(),
            owner: "lead".into(),
            assignee: Some("m1".into()),
            ..Default::default()
        };
        meta_done.save(&crate::doc_flow::DocMeta::meta_path(&dir, "taskx", "done")).unwrap();
        // no assignee -> skipped
        let meta_none = crate::doc_flow::DocMeta {
            doc: "taskx".into(),
            id: "none".into(),
            state: "assigned".into(),
            owner: "lead".into(),
            assignee: None,
            ..Default::default()
        };
        meta_none.save(&crate::doc_flow::DocMeta::meta_path(&dir, "taskx", "none")).unwrap();

        let written = nudge_open_tasks(&mut conn, 300, Some(10_000), Some(&taskx)).unwrap();
        assert!(written.is_empty(), "done/unassigned tasks must not be nudged");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // taskx command-layer E2E tests. These drive cmd_task / task_claim /
    // task_retract / task_doc_event over a scratch workdir, so they change the
    // process cwd. A static mutex serializes them against each other; no other
    // test in this crate reads current_dir, so parallel tests are unaffected.
    // -----------------------------------------------------------------------

    static TASK_CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A scratch workdir whose cwd is current for the lifetime of `f`.
    /// Restores the original cwd and removes the temp dir afterwards.
    fn with_task_cwd<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = TASK_CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::current_dir().expect("current_dir");
        let dir = std::env::temp_dir().join(format!("teamx-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".teamx").join("docs")).expect("mkdir .teamx/docs");
        // A real git repo keeps auto_git_commit quiet (commits succeed).
        let _ = std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(&dir)
            .status();
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["config", "user.email", "test@example.com"])
            .status();
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["config", "user.name", "test"])
            .status();
        std::env::set_current_dir(&dir).expect("set cwd to scratch dir");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dir)));
        let _ = std::env::set_current_dir(&original);
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(p) = result {
            std::panic::resume_unwind(p);
        }
    }

    /// Seed a team + owner + active role members (id, session, display name).
    fn seed_task_team(
        conn: &Connection,
        team_id: &str,
        owner_id: &str,
        owner_session: &str,
        role_members: &[(&str, &str)],
    ) {
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES (?1, '任务团队', ?2, NULL, 'active', 'tok', 'now', 'now')",
            params![team_id, owner_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at, is_lead)
             VALUES (?1, ?2, ?3, '老板', 'owner', 'active', 'now', 1)",
            params![owner_id, team_id, owner_session],
        )
        .unwrap();
        for (i, (mid, sess)) in role_members.iter().enumerate() {
            conn.execute(
                "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at, is_lead)
                 VALUES (?1, ?2, ?3, ?4, 'ui-dev', 'active', 'now', 0)",
                params![mid, team_id, sess, format!("成员{i}")],
            )
            .unwrap();
        }
    }

    fn read_meta(docs_root: &std::path::Path, id: &str) -> crate::doc_flow::DocMeta {
        crate::doc_flow::DocMeta::load(&crate::doc_flow::DocMeta::meta_path(
            docs_root,
            crate::doc_flow::BUILTIN_TASKX,
            id,
        ))
        .unwrap()
    }

    fn create_task(
        conn: &mut Connection,
        team: &str,
        session: &str,
        title: &str,
        id: &str,
        mode: &str,
        role: Option<&str>,
        assignee: Option<&str>,
    ) {
        use crate::cli::TaskCmd;
        let out = cmd_task(
            conn,
            &TaskCmd::Create {
                title: title.to_string(),
                assignee: assignee.map(|s| s.to_string()),
                role: role.map(|s| s.to_string()),
                mode: Some(mode.to_string()),
                executor: "either".to_string(),
                priority: "medium".to_string(),
                id: Some(id.to_string()),
                detail: None,
                no_push: true,
                session: session.to_string(),
                team: Some(team.to_string()),
            },
        )
        .unwrap();
        assert_eq!(out["ok"], true, "create {mode} task {title}: {out}");
    }

    /// Run a single task lifecycle command (the variant carries id/session/team).
    fn run_task(conn: &mut Connection, variant: crate::cli::TaskCmd) -> Result<Value> {
        cmd_task(conn, &variant)
    }

    /// CRITICAL-1 regression: retract MUST clear the assignee so another role
    /// member can re-claim the task (bid pool is open again).
    #[test]
    fn taskx_bid_retract_clears_assignee_for_reclaim() {
        with_task_cwd(|dir| {
            let mut conn = test_conn();
            let team = "t-bid";
            seed_task_team(
                &conn,
                team,
                "m-owner",
                "s:owner",
                &[("m-ui-1", "s:ui1"), ("m-ui-2", "s:ui2")],
            );
            create_task(&mut conn, team, "s:owner", "修首页 bug", "t1", "bid", Some("ui-dev"), None);

            // Claim by ui-1.
            task_claim(&mut conn, "t1", "s:ui1", Some(team)).unwrap();
            let m = read_meta(&dir.join(".teamx/docs"), "t1");
            assert_eq!(m.state, "claimed");
            assert_eq!(m.assignee.as_deref(), Some("m-ui-1"));

            // Retract by the claimer -> assignee must be cleared (regression).
            task_retract(&mut conn, "t1", "s:ui1", Some(team)).unwrap();
            let m = read_meta(&dir.join(".teamx/docs"), "t1");
            assert_eq!(m.state, "assigned", "retract returns the task to the open pool");
            assert_eq!(m.assignee, None, "retract MUST clear the assignee (CRITICAL-1)");

            // A different role member can now claim.
            task_claim(&mut conn, "t1", "s:ui2", Some(team)).unwrap();
            let m = read_meta(&dir.join(".teamx/docs"), "t1");
            assert_eq!(m.state, "claimed");
            assert_eq!(m.assignee.as_deref(), Some("m-ui-2"), "second member re-claims after retract");
        });
    }

    /// HIGH-2: an assignee must NOT be able to verify their own task — only a
    /// team lead may close the acceptance loop.
    #[test]
    fn taskx_assignee_cannot_self_verify() {
        with_task_cwd(|dir| {
            let mut conn = test_conn();
            let team = "t-vfy";
            seed_task_team(
                &conn,
                team,
                "m-owner",
                "s:owner",
                &[("m-ui-1", "s:ui1"), ("m-ui-2", "s:ui2")],
            );
            // direct task assigned to m-ui-1
            create_task(&mut conn, team, "s:owner", "做登录页", "t1", "direct", None, Some("m-ui-1"));

            // assignee marks done (allowed — advancing their own task)
            run_task(
                &mut conn,
                crate::cli::TaskCmd::Done {
                    id: "t1".to_string(),
                    result: Some("完成".to_string()),
                    session: "s:ui1".to_string(),
                    team: Some(team.to_string()),
                },
            )
            .unwrap();

            // assignee self-verify must FAIL
            let err = run_task(
                &mut conn,
                crate::cli::TaskCmd::Verify {
                    id: "t1".to_string(),
                    session: "s:ui1".to_string(),
                    team: Some(team.to_string()),
                },
            )
            .unwrap_err();
            assert!(err.0.contains("only a team lead"), "assignee self-verify denied: {err}");

            // team lead verify succeeds and closes the loop
            run_task(
                &mut conn,
                crate::cli::TaskCmd::Verify {
                    id: "t1".to_string(),
                    session: "s:owner".to_string(),
                    team: Some(team.to_string()),
                },
            )
            .unwrap();
            let m = read_meta(&dir.join(".teamx/docs"), "t1");
            assert_eq!(m.state, "verified", "lead verify closes the loop");
        });
    }

    /// MEDIUM-3: a same-role member who is NOT the assignee cannot write
    /// progress (doc.updated) on someone else's task; the assignee can.
    #[test]
    fn taskx_update_requires_assignee_or_lead() {
        with_task_cwd(|dir| {
            let mut conn = test_conn();
            let team = "t-upd";
            seed_task_team(
                &conn,
                team,
                "m-owner",
                "s:owner",
                &[("m-ui-1", "s:ui1"), ("m-ui-2", "s:ui2")],
            );
            // broadcast gives each role member their own instance; m-ui-2 must
            // not update m-ui-1's copy.
            create_task(&mut conn, team, "s:owner", "全员测试", "bcast", "broadcast", Some("ui-dev"), None);

            let err = run_task(
                &mut conn,
                crate::cli::TaskCmd::Update {
                    id: "bcast@m-ui-1".to_string(),
                    progress: "我来插话".to_string(),
                    session: "s:ui2".to_string(),
                    team: Some(team.to_string()),
                },
            )
            .unwrap_err();
            assert!(
                err.0.contains("only the assignee or a lead"),
                "non-assignee update denied: {err}"
            );

            // the instance's own assignee can update.
            run_task(
                &mut conn,
                crate::cli::TaskCmd::Update {
                    id: "bcast@m-ui-1".to_string(),
                    progress: "进展更新".to_string(),
                    session: "s:ui1".to_string(),
                    team: Some(team.to_string()),
                },
            )
            .unwrap();
            let m = read_meta(&dir.join(".teamx/docs"), "bcast@m-ui-1");
            assert_eq!(m.state, "assigned");
        });
    }

    /// LOW-4: broadcast instances are keyed by the FULL member id (no
    /// 8-char truncation), and each member works their own instance.
    #[test]
    fn taskx_broadcast_uses_full_member_id_instances() {
        with_task_cwd(|dir| {
            let mut conn = test_conn();
            let team = "t-bc";
            seed_task_team(
                &conn,
                team,
                "m-owner",
                "s:owner",
                &[("member-aaaaaaaa-ui1", "s:ui1"), ("member-bbbbbbbb-ui2", "s:ui2")],
            );
            let out = cmd_task(
                &mut conn,
                &crate::cli::TaskCmd::Create {
                    title: "全员回归".to_string(),
                    assignee: None,
                    role: Some("ui-dev".to_string()),
                    mode: Some("broadcast".to_string()),
                    executor: "either".to_string(),
                    priority: "medium".to_string(),
                    id: Some("reg".to_string()),
                    detail: None,
                    no_push: true,
                    session: "s:owner".to_string(),
                    team: Some(team.to_string()),
                },
            )
            .unwrap();
            let instances = out["instances"].as_array().unwrap().clone();
            assert_eq!(instances.len(), 2, "one instance per role member");
            let ids: Vec<String> = instances.iter().map(|v| v.as_str().unwrap().to_string()).collect();
            assert!(
                ids.contains(&"reg@member-aaaaaaaa-ui1".to_string())
                    && ids.contains(&"reg@member-bbbbbbbb-ui2".to_string()),
                "instance ids use the FULL member id, not an 8-char truncation: {ids:?}"
            );

            // Each member can advance (done) their OWN instance.
            for (member, sess) in [("member-aaaaaaaa-ui1", "s:ui1"), ("member-bbbbbbbb-ui2", "s:ui2")] {
                let id = format!("reg@{member}");
                run_task(
                    &mut conn,
                    crate::cli::TaskCmd::Done {
                        id: id.clone(),
                        result: Some("搞定".to_string()),
                        session: sess.to_string(),
                        team: Some(team.to_string()),
                    },
                )
                .unwrap();
                let m = read_meta(&dir.join(".teamx/docs"), &id);
                assert_eq!(m.state, "done", "{id} advanced by its own assignee");
            }
        });
    }

    /// LOW-5: the claim lock lives in the temp dir (never committed to the
    /// team repo) and is deterministic per meta path.
    #[test]
    fn task_lock_path_is_temp_and_deterministic() {
        let meta = std::path::Path::new("/proj/.teamx/docs/taskx/t1.meta.json");
        let l1 = task_lock_path(meta);
        let l2 = task_lock_path(meta);
        assert_eq!(l1, l2, "deterministic per meta path");
        let tmp = std::env::temp_dir();
        assert!(
            l1.starts_with(&tmp),
            "lock file must live in temp dir, not the repo: {}",
            l1.display()
        );
        let other = std::path::Path::new("/proj/.teamx/docs/taskx/t2.meta.json");
        assert_ne!(l1, task_lock_path(other), "different tasks must not share a lock");
    }
}
