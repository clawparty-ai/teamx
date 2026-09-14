//! IM identity binding (IM P1) — one-time binding codes + the binding tables.
//!
//! Design: `docs/design/im-access-requirements.md` §5.2 (DQ-0003) and §6.
//!
//! * A **binding code** is created over an authenticated channel and is
//!   **pre-associated with a `member_id`** (one-way authorization: an
//!   authenticated member authorizes an unverified IM identity). Self-service
//!   codes need no lead approval.
//! * Codes are ≥8 characters of CSPRNG output, TTL 5 minutes, **single use**
//!   and stored as a **hash only** — the plaintext code never touches the DB.
//! * The **binding table gives identity, the `members` table gives authority**:
//!   roles are read live from `members`, never cached in a binding.
//! * Repeated failures for one IM user are throttled (5 failures → 15 minutes)
//!   and every rejection is written to the ledger as `im.rejected`, so an
//!   enumeration attempt is auditable.
//!
//! Ledger events (design §7): `im.binding` on bind / rebind / unbind,
//! `im.rejected` with a frozen `reason_code` (`unbound` / `forbidden` /
//! `duplicate` / `invalid_signature`) plus a finer `detail`.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::db;
use crate::events;
use crate::im::types::ImDelegatedContext;

/// Code alphabet: 30 unambiguous characters (no `0/1/I/L/O/U`), so a code can
/// be dictated over IM without transcription errors.
const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_LEN: usize = 10;
/// Reject absurd input before hashing (cheap DoS guard, never a valid code).
const CODE_MIN_LEN: usize = 8;
const CODE_MAX_INPUT_LEN: usize = 64;
const CODE_TTL_SECS: i64 = 300;
const CODE_TTL_MIN_SECS: i64 = 60;
const CODE_TTL_MAX_SECS: i64 = 900;
const RATE_LIMIT_FAILURES: i64 = 5;
const RATE_LIMIT_WINDOW_SECS: i64 = 900;
/// Attempt rows older than this are pruned on write (bounded table).
const ATTEMPT_RETENTION_SECS: i64 = 86_400;

/// SHA-256 hex of a code. Only this value is stored.
pub fn hash_code(code: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, code.trim().as_bytes());
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh code from the OS CSPRNG (rejection sampling — no modulo bias).
fn random_code() -> Result<String, String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let rng = SystemRandom::new();
    let mut out = String::with_capacity(CODE_LEN);
    while out.len() < CODE_LEN {
        let mut buf = [0u8; 32];
        rng.fill(&mut buf)
            .map_err(|_| "im.bind_code: CSPRNG unavailable".to_string())?;
        for b in buf {
            // 240 = 8 * 30: values above it are discarded to keep the
            // distribution uniform over the alphabet.
            if b >= 240 {
                continue;
            }
            out.push(ALPHABET[(b % ALPHABET.len() as u8) as usize] as char);
            if out.len() == CODE_LEN {
                break;
            }
        }
    }
    Ok(out)
}

fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

fn now() -> String {
    events::db_now()
}

// ---------------------------------------------------------------------------
// Ledger side effects
// ---------------------------------------------------------------------------

/// Record a rejection: ledger event (when the team is known) + throttling row.
///
/// Returns the caller-facing error message.
fn reject(
    conn: &mut Connection,
    platform: &str,
    im_user_id: &str,
    team_id: Option<&str>,
    reason_code: &str,
    detail: &str,
) -> String {
    let _ = record_attempt(conn, platform, im_user_id, false);
    if let Some(team_id) = team_id {
        let payload = json!({
            "platform": platform,
            "im_user": im_user_id,
            "reason_code": reason_code,
            "detail": detail,
            "action": "bind",
            "source": format!("im:{platform}"),
        });
        let tid = team_id.to_string();
        let _ = db::with_write(conn, |tx| {
            events::emit(tx, &tid, None, "im.rejected", Some(&payload))?;
            Ok(())
        });
    }
    format!("im.bind rejected ({reason_code}/{detail})")
}

/// Record one binding attempt for throttling.
fn record_attempt(
    conn: &mut Connection,
    platform: &str,
    im_user_id: &str,
    ok: bool,
) -> Result<(), String> {
    let epoch = chrono::Utc::now().timestamp();
    db::with_write(conn, |tx| {
        tx.execute(
            "INSERT INTO im_bind_attempts (platform, im_user_id, attempted_at, ok)
             VALUES (?1, ?2, ?3, ?4)",
            params![platform, im_user_id, epoch, ok as i64],
        )?;
        tx.execute(
            "DELETE FROM im_bind_attempts WHERE attempted_at < ?1",
            params![epoch - ATTEMPT_RETENTION_SECS],
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))
}

fn recent_failures(
    conn: &Connection,
    platform: &str,
    im_user_id: &str,
    window_secs: i64,
) -> Result<i64, String> {
    let since = chrono::Utc::now().timestamp() - window_secs;
    conn.query_row(
        "SELECT COUNT(*) FROM im_bind_attempts
         WHERE platform = ?1 AND im_user_id = ?2 AND ok = 0 AND attempted_at > ?3",
        params![platform, im_user_id, since],
        |r| r.get::<_, i64>(0),
    )
    .map_err(|e| format!("db: {e}"))
}

// ---------------------------------------------------------------------------
// Membership / role lookup
// ---------------------------------------------------------------------------

/// `(display_name, role)` of an **active** member of the team; `None` when the
/// member is unknown, or `left`/`denied` (bound seats are invalidated by the
/// state, without touching the binding rows — design §5.2).
pub fn active_member(
    conn: &Connection,
    team_id: &str,
    member_id: &str,
) -> Result<Option<(String, Option<String>)>, String> {
    conn.query_row(
        "SELECT display_name, role FROM members
         WHERE id = ?1 AND team_id = ?2 AND state NOT IN ('left','denied')",
        params![member_id, team_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
    )
    .optional()
    .map_err(|e| format!("db: {e}"))
}

/// Lead check (primary owner or promoted co-lead), mirroring `server_tasks`.
pub fn is_lead(conn: &Connection, team_id: &str, member_id: &str) -> Result<bool, String> {
    let row: Option<(Option<String>, bool)> = conn
        .query_row(
            "SELECT t.owner_member_id, m.is_lead
             FROM teams t JOIN members m ON m.team_id = t.id
             WHERE t.id = ?1 AND m.id = ?2 AND m.state NOT IN ('left','denied')",
            params![team_id, member_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    Ok(row
        .map(|(owner, is_lead)| owner.as_deref() == Some(member_id) || is_lead)
        .unwrap_or(false))
}

// ---------------------------------------------------------------------------
// Code creation
// ---------------------------------------------------------------------------

/// Create a one-time binding code for `member_id` in `team_id`.
///
/// The plaintext code is returned exactly once (it is not recoverable later).
pub fn create_code(
    conn: &mut Connection,
    member_id: &str,
    team_id: &str,
    ttl_secs: Option<i64>,
) -> Result<Value, String> {
    let member = active_member(conn, team_id, member_id)?
        .ok_or_else(|| format!("im.bind_code: `{member_id}` is not an active member of team {team_id}"))?;
    let ttl = ttl_secs.unwrap_or(CODE_TTL_SECS).clamp(CODE_TTL_MIN_SECS, CODE_TTL_MAX_SECS);
    let code = random_code()?;
    let code_hash = hash_code(&code);
    let created_at = now();
    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(ttl)).to_rfc3339();

    let (hash, created, expires) = (code_hash.clone(), created_at.clone(), expires_at.clone());
    let (mid, tid) = (member_id.to_string(), team_id.to_string());
    db::with_write(conn, move |tx| {
        tx.execute(
            "INSERT INTO im_bind_codes
               (code_hash, member_id, team_id, scope, created_by, created_at, expires_at, used_at)
             VALUES (?1, ?2, ?3, 'self', ?2, ?4, ?5, NULL)",
            params![hash, mid, tid, created, expires],
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))?;

    Ok(json!({
        "ok": true,
        "code": code,
        "code_len": CODE_LEN,
        "team_id": team_id,
        "member_id": member_id,
        "member": { "id": member_id, "display_name": member.0, "role": member.1 },
        "created_at": created_at,
        "expires_at": expires_at,
        "ttl_secs": ttl,
        "hint": "send this code to the team bot from your IM account; it is single-use",
    }))
}

// ---------------------------------------------------------------------------
// Redemption (the gateway path)
// ---------------------------------------------------------------------------

/// Redeem a binding code: bind (or rebind) `ctx.im_user_id` to the member the
/// code was created for, and echo that member's role.
///
/// In P1 the IM gateway runs in-process and calls this directly (design §5.3
/// option A); when the gateway becomes a separate process (P2) it must verify
/// the provider signature before delegating — this function intentionally does
/// no platform I/O.
pub fn redeem(conn: &mut Connection, ctx: &ImDelegatedContext, code: &str) -> Result<Value, String> {
    redeem_with_team(conn, ctx, code, None)
}

/// [`redeem`] with an expected team: an actor acting inside team X may only
/// redeem a code issued for X (used by the CLI/RPC paths, where the actor's
/// team comes from the session or the client certificate). A cross-team attempt
/// counts as a failure and is written to the ledger as `im.rejected`.
pub fn redeem_in_team(
    conn: &mut Connection,
    ctx: &ImDelegatedContext,
    code: &str,
    team_id: &str,
) -> Result<Value, String> {
    redeem_with_team(conn, ctx, code, Some(team_id))
}

fn redeem_with_team(
    conn: &mut Connection,
    ctx: &ImDelegatedContext,
    code: &str,
    expected_team: Option<&str>,
) -> Result<Value, String> {
    let platform = ctx.platform.trim().to_string();
    let im_user_id = ctx.im_user_id.trim().to_string();
    // Rejections before the code is known cannot be attributed to the code's
    // team, so they are audited against the actor's team when one is known
    // (CLI/RPC paths always pass it); the in-process gateway passes `None` and
    // the rejection only surfaces as its return value.
    let audit_team = expected_team;
    if platform.is_empty() || im_user_id.is_empty() {
        return Err("im.bind: `platform` and `im_user_id` are required".to_string());
    }
    let code = code.trim().to_string();
    if code.len() < CODE_MIN_LEN || code.len() > CODE_MAX_INPUT_LEN {
        return Err(reject(
            conn,
            &platform,
            &im_user_id,
            audit_team,
            "unbound",
            "code_format",
        ));
    }

    if recent_failures(conn, &platform, &im_user_id, RATE_LIMIT_WINDOW_SECS)? >= RATE_LIMIT_FAILURES {
        return Err(reject(
            conn,
            &platform,
            &im_user_id,
            audit_team,
            "unbound",
            "rate_limited",
        ));
    }

    let hash = hash_code(&code);
    let row: Option<(String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT member_id, team_id, expires_at, used_at FROM im_bind_codes WHERE code_hash = ?1",
            params![hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;

    let Some((member_id, team_id, expires_at, used_at)) = row else {
        return Err(reject(
            conn,
            &platform,
            &im_user_id,
            audit_team,
            "unbound",
            "code_unknown",
        ));
    };

    if used_at.is_some() {
        return Err(reject(
            conn,
            &platform,
            &im_user_id,
            Some(&team_id),
            "unbound",
            "code_used",
        ));
    }
    if let Some(expected) = expected_team {
        if expected != team_id {
            return Err(reject(
                conn,
                &platform,
                &im_user_id,
                Some(&team_id),
                "forbidden",
                "team_mismatch",
            ));
        }
    }
    if parse_ts(&expires_at).map(|e| e <= chrono::Utc::now()).unwrap_or(true) {
        return Err(reject(
            conn,
            &platform,
            &im_user_id,
            Some(&team_id),
            "unbound",
            "code_expired",
        ));
    }
    let Some(member) = active_member(conn, &team_id, &member_id)? else {
        return Err(reject(
            conn,
            &platform,
            &im_user_id,
            Some(&team_id),
            "forbidden",
            "member_inactive",
        ));
    };

    // Existing identities, for the bind/rebind decision + audit trail. Both
    // directions are cleaned inside the transaction, so one IM user maps to one
    // member and one member holds one IM user per team ("换绑").
    let previous_for_user = active_binding_member(conn, &platform, &im_user_id, &team_id)?;
    let previous_for_member = active_binding_for_member(conn, &platform, &team_id, &member_id)?;
    let action = if previous_for_user.is_some() || previous_for_member.is_some() {
        "rebind"
    } else {
        "bind"
    };

    let bound_at = now();
    let (p, u, mid, tid, b_at, act) = (
        platform.clone(),
        im_user_id.clone(),
        member_id.clone(),
        team_id.clone(),
        bound_at.clone(),
        action.to_string(),
    );
    let payload = json!({
        "platform": p,
        "im_user": u,
        "member_id": mid,
        "team_id": tid,
        "action": act,
        "bound_at": b_at,
        "source": format!("im:{p}"),
        "previous_member_id": previous_for_user,
        "previous_im_user_id": previous_for_member,
    });
    let (h, created) = (hash.clone(), bound_at.clone());
    let consumed = db::with_write(conn, |tx| {
        // Single-use guard: only the transaction that flips `used_at` wins.
        let changed = tx.execute(
            "UPDATE im_bind_codes SET used_at = ?1 WHERE code_hash = ?2 AND used_at IS NULL",
            params![created, h],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        // a member keeps exactly one IM user per platform+team
        tx.execute(
            "UPDATE im_bindings SET revoked_at = ?1
             WHERE platform = ?2 AND team_id = ?3 AND member_id = ?4 AND revoked_at IS NULL",
            params![created, p, tid, mid],
        )?;
        // The (platform, im_user, team) triple is the identity of a binding, so
        // rebinding replaces that row: the old member keeps no rights once the
        // code is redeemed elsewhere (the ledger event keeps the history).
        tx.execute(
            "INSERT INTO im_bindings
               (platform, im_user_id, member_id, team_id, bound_at, bound_by, revoked_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'im-gateway', NULL, ?5)
             ON CONFLICT(platform, im_user_id, team_id) DO UPDATE SET
               member_id    = excluded.member_id,
               bound_at     = excluded.bound_at,
               bound_by     = excluded.bound_by,
               revoked_at   = NULL,
               last_seen_at = excluded.last_seen_at",
            params![p, u, mid, tid, created],
        )?;
        events::emit(tx, &tid, Some(&mid), "im.binding", Some(&payload))?;
        Ok(true)
    })
    .map_err(|e| format!("db: {e}"))?;

    if !consumed {
        return Err(reject(
            conn,
            &platform,
            &im_user_id,
            Some(&team_id),
            "unbound",
            "code_used",
        ));
    }
    record_attempt(conn, &platform, &im_user_id, true)?;

    Ok(json!({
        "ok": true,
        "action": action,
        "platform": platform,
        "im_user_id": im_user_id,
        "team_id": team_id,
        "member": { "id": member_id, "display_name": member.0, "role": member.1 },
        "bound_at": bound_at,
        "previous": {
            "member_id": previous_for_user,
            "im_user_id": previous_for_member,
        },
    }))
}

/// Member currently bound to this IM identity (active bindings only).
pub fn active_binding_member(
    conn: &Connection,
    platform: &str,
    im_user_id: &str,
    team_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT member_id FROM im_bindings
         WHERE platform = ?1 AND im_user_id = ?2 AND team_id = ?3 AND revoked_at IS NULL",
        params![platform, im_user_id, team_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| format!("db: {e}"))
}

/// IM identity currently bound to this member (active bindings only).
fn active_binding_for_member(
    conn: &Connection,
    platform: &str,
    team_id: &str,
    member_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT im_user_id FROM im_bindings
         WHERE platform = ?1 AND team_id = ?2 AND member_id = ?3 AND revoked_at IS NULL",
        params![platform, team_id, member_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| format!("db: {e}"))
}

// ---------------------------------------------------------------------------
// Unbind
// ---------------------------------------------------------------------------

/// Unbind an IM identity. Self-service for the bound member; a team lead may
/// unbind anyone in the team.
pub fn unbind(
    conn: &mut Connection,
    actor_member_id: &str,
    team_id: &str,
    platform: &str,
    im_user_id: &str,
) -> Result<Value, String> {
    let platform = platform.trim().to_string();
    let im_user_id = im_user_id.trim().to_string();
    if platform.is_empty() || im_user_id.is_empty() {
        return Err("im.unbind: `platform` and `im_user_id` are required".to_string());
    }
    let bound = active_binding_member(conn, &platform, &im_user_id, team_id)?;
    let Some(bound_member) = bound else {
        return Err(format!(
            "im.unbind: no active binding for {platform}:{im_user_id} in team {team_id}"
        ));
    };
    if bound_member != actor_member_id && !is_lead(conn, team_id, actor_member_id)? {
        let _ = record_attempt(conn, &platform, &im_user_id, false);
        return Err(reject(
            conn,
            &platform,
            &im_user_id,
            Some(team_id),
            "forbidden",
            "unbind_not_self_or_lead",
        ));
    }

    let at = now();
    let payload = json!({
        "platform": platform,
        "im_user": im_user_id,
        "member_id": bound_member,
        "team_id": team_id,
        "action": "unbind",
        "bound_at": at,
        "by": actor_member_id,
        "source": format!("im:{platform}"),
    });
    let (p, u, tid, a) = (
        platform.clone(),
        im_user_id.clone(),
        team_id.to_string(),
        at.clone(),
    );
    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE im_bindings SET revoked_at = ?1
             WHERE platform = ?2 AND im_user_id = ?3 AND team_id = ?4 AND revoked_at IS NULL",
            params![a, p, u, tid],
        )?;
        events::emit(tx, &tid, Some(&bound_member), "im.binding", Some(&payload))?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))?;

    Ok(json!({
        "ok": true,
        "unbound": true,
        "platform": platform,
        "im_user_id": im_user_id,
        "member_id": bound_member,
        "team_id": team_id,
        "revoked_at": at,
    }))
}

// ---------------------------------------------------------------------------
// Lookup / listing
// ---------------------------------------------------------------------------

/// Resolve the member behind an inbound IM identity and refresh `last_seen_at`.
///
/// This is the identity source used by the inbound gateway (design §5.3: the
/// gateway never carries a `member_id`). Returns `None` when unbound, revoked,
/// or when the member has left / been denied — the caller then answers
/// `im.unbound` and writes an `im.rejected` event.
pub fn resolve_member(
    conn: &mut Connection,
    platform: &str,
    im_user_id: &str,
    team_id: &str,
) -> Result<Option<Value>, String> {
    let member_id = active_binding_member(conn, platform, im_user_id, team_id)?;
    let Some(member_id) = member_id else {
        return Ok(None);
    };
    let Some(member) = active_member(conn, team_id, &member_id)? else {
        return Ok(None);
    };
    let at = now();
    let (p, u, tid, seen) = (
        platform.to_string(),
        im_user_id.to_string(),
        team_id.to_string(),
        at.clone(),
    );
    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE im_bindings SET last_seen_at = ?1
             WHERE platform = ?2 AND im_user_id = ?3 AND team_id = ?4 AND revoked_at IS NULL",
            params![seen, p, u, tid],
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))?;
    Ok(Some(json!({
        "member_id": member_id,
        "display_name": member.0,
        "role": member.1,
        "team_id": team_id,
        "platform": platform,
        "im_user_id": im_user_id,
        "last_seen_at": at,
    })))
}

const SELECT_BINDING: &str = "SELECT b.platform, b.im_user_id, b.member_id, b.team_id, b.bound_at,
       b.bound_by, b.revoked_at, b.last_seen_at, m.display_name, m.role
  FROM im_bindings b LEFT JOIN members m ON m.id = b.member_id AND m.team_id = b.team_id";

fn row_to_binding(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "platform": r.get::<_, String>(0)?,
        "im_user_id": r.get::<_, String>(1)?,
        "member_id": r.get::<_, String>(2)?,
        "team_id": r.get::<_, String>(3)?,
        "bound_at": r.get::<_, String>(4)?,
        "bound_by": r.get::<_, String>(5)?,
        "revoked_at": r.get::<_, Option<String>>(6)?,
        "last_seen_at": r.get::<_, Option<String>>(7)?,
        "display_name": r.get::<_, Option<String>>(8)?,
        "role": r.get::<_, Option<String>>(9)?,
    }))
}

/// Active bindings of `member_id` (optionally limited to one team).
pub fn bindings_for_member(
    conn: &Connection,
    member_id: &str,
    team_id: Option<&str>,
) -> Result<Vec<Value>, String> {
    match team_id {
        Some(tid) => {
            let mut stmt = conn
                .prepare(&format!(
                    "{SELECT_BINDING} WHERE b.member_id = ?1 AND b.team_id = ?2 AND b.revoked_at IS NULL
                     ORDER BY b.bound_at ASC"
                ))
                .map_err(|e| format!("db: {e}"))?;
            let rows = stmt
                .query_map(params![member_id, tid], row_to_binding)
                .map_err(|e| format!("db: {e}"))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| format!("db: {e}"))
        }
        None => {
            let mut stmt = conn
                .prepare(&format!(
                    "{SELECT_BINDING} WHERE b.member_id = ?1 AND b.revoked_at IS NULL ORDER BY b.bound_at ASC"
                ))
                .map_err(|e| format!("db: {e}"))?;
            let rows = stmt
                .query_map(params![member_id], row_to_binding)
                .map_err(|e| format!("db: {e}"))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| format!("db: {e}"))
        }
    }
}

/// Every active binding of a team (lead view).
pub fn bindings_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT_BINDING} WHERE b.team_id = ?1 AND b.revoked_at IS NULL ORDER BY b.bound_at ASC"
        ))
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![team_id], row_to_binding)
        .map_err(|e| format!("db: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("db: {e}"))
}

/// `im.bindings.list` core: own bindings, or the whole team for a lead.
pub fn list_bindings(
    conn: &Connection,
    member_id: &str,
    team_id: &str,
    all: bool,
) -> Result<Value, String> {
    if all && !is_lead(conn, team_id, member_id)? {
        return Err("im.bindings: only the team owner or a co-lead may list all bindings".to_string());
    }
    let bindings = if all {
        bindings_for_team(conn, team_id)?
    } else {
        bindings_for_member(conn, member_id, Some(team_id))?
    };
    Ok(json!({ "ok": true, "team_id": team_id, "scope": if all { "team" } else { "self" },
               "count": bindings.len(), "bindings": bindings }))
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

    /// One team, an owner + a plain member. Returns `(conn, team_id)`.
    fn seed() -> (Connection, String) {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES ('t1','T','m-owner',NULL,'active','tok','now','now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, is_lead, joined_at)
             VALUES ('m-owner','t1','s-owner','Owner','owner','active',1,'now'),
                    ('m-worker','t1','s-worker','Worker','contributor','active',0,'now')",
            [],
        )
        .unwrap();
        (conn, "t1".to_string())
    }

    fn ctx(im_user: &str) -> ImDelegatedContext {
        ImDelegatedContext {
            platform: "feishu".to_string(),
            im_user_id: im_user.to_string(),
            im_team: None,
            signature: None,        timestamp: 0,
        nonce: String::new(),
        }
    }

    /// Codes are single-use, so tests that only need a *used* code path can
    /// reach into the table directly.
    fn expire_code(conn: &Connection, code: &str) {
        conn.execute(
            "UPDATE im_bind_codes SET expires_at = '2000-01-01T00:00:00+00:00' WHERE code_hash = ?1",
            params![hash_code(code)],
        )
        .unwrap();
    }

    fn binding_events(conn: &Connection) -> Vec<Value> {
        let mut stmt = conn
            .prepare("SELECT payload_json FROM events WHERE type = 'im.binding' ORDER BY seq ASC")
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, Option<String>>(0))
            .unwrap();
        rows.map(|r| {
            let raw = r.unwrap().unwrap();
            let v: Value = serde_json::from_str(&raw).unwrap();
            json!({ "action": v["action"].clone(), "member_id": v["member_id"].clone(), "im_user": v["im_user"].clone() })
        })
        .collect()
    }

    #[test]
    fn code_create_requires_active_membership() {
        let (mut conn, _) = seed();
        let err = create_code(&mut conn, "m-ghost", "t1", None).unwrap_err();
        assert!(err.contains("not an active member"), "{err}");
        // the owner may create one for themselves
        let ok = create_code(&mut conn, "m-owner", "t1", None).unwrap();
        assert_eq!(ok["code"].as_str().unwrap().len(), CODE_LEN);
        assert_eq!(ok["ttl_secs"], json!(CODE_TTL_SECS));
    }

    #[test]
    fn redeem_binds_and_echoes_role() {
        let (mut conn, _) = seed();
        let created = create_code(&mut conn, "m-worker", "t1", None).unwrap();
        let code = created["code"].as_str().unwrap().to_string();

        let out = redeem(&mut conn, &ctx("ou_worker"), &code).unwrap();
        assert_eq!(out["action"], json!("bind"));
        assert_eq!(out["member"]["id"], json!("m-worker"));
        assert_eq!(out["member"]["role"], json!("contributor"));
        assert_eq!(out["member"]["display_name"], json!("Worker"));

        // identity lookup (what the gateway uses) resolves without a member_id
        let who = resolve_member(&mut conn, "feishu", "ou_worker", "t1").unwrap().unwrap();
        assert_eq!(who["member_id"], json!("m-worker"));
        assert!(who["last_seen_at"].is_string());
        assert_eq!(
            binding_events(&conn),
            vec![json!({ "action": "bind", "member_id": "m-worker", "im_user": "ou_worker" })]
        );
    }

    #[test]
    fn code_is_single_use() {
        let (mut conn, _) = seed();
        let code = create_code(&mut conn, "m-worker", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        redeem(&mut conn, &ctx("ou_worker"), &code).unwrap();

        let err = redeem(&mut conn, &ctx("ou_other"), &code).unwrap_err();
        assert!(err.contains("code_used"), "{err}");
        // no second binding appeared
        assert_eq!(bindings_for_team(&conn, "t1").unwrap().len(), 1);
        // and the rejection is on the ledger
        let rejected: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE type = 'im.rejected'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rejected, 1);
    }

    #[test]
    fn expired_code_is_rejected() {
        let (mut conn, _) = seed();
        let code = create_code(&mut conn, "m-worker", "t1", Some(60)).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        expire_code(&conn, &code);
        let err = redeem(&mut conn, &ctx("ou_worker"), &code).unwrap_err();
        assert!(err.contains("code_expired"), "{err}");
        assert!(bindings_for_team(&conn, "t1").unwrap().is_empty());
    }

    #[test]
    fn unknown_codes_are_rate_limited_after_five_failures() {
        let (mut conn, _) = seed();
        for _ in 0..RATE_LIMIT_FAILURES {
            let err = redeem(&mut conn, &ctx("ou_attacker"), "AAAAAAAAAA").unwrap_err();
            assert!(err.contains("code_unknown"), "{err}");
        }
        // 6th attempt: throttled even though a *valid* code is presented now
        let good = create_code(&mut conn, "m-worker", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        let err = redeem(&mut conn, &ctx("ou_attacker"), &good).unwrap_err();
        assert!(err.contains("rate_limited"), "{err}");
        // throttling is per IM identity: another user is unaffected
        let ok = redeem(&mut conn, &ctx("ou_worker"), &good).unwrap();
        assert_eq!(ok["member"]["id"], json!("m-worker"));
    }

    #[test]
    fn short_or_oversized_code_input_is_rejected_without_a_lookup() {
        let (mut conn, _) = seed();
        assert!(redeem(&mut conn, &ctx("ou_x"), "123").unwrap_err().contains("code_format"));
        let long = "A".repeat(CODE_MAX_INPUT_LEN + 1);
        assert!(redeem(&mut conn, &ctx("ou_x"), &long).unwrap_err().contains("code_format"));
    }

    #[test]
    fn rebinding_an_im_user_moves_the_binding_to_the_new_member() {
        let (mut conn, _) = seed();
        let c1 = create_code(&mut conn, "m-worker", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        redeem(&mut conn, &ctx("ou_shared"), &c1).unwrap();

        let c2 = create_code(&mut conn, "m-owner", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        let out = redeem(&mut conn, &ctx("ou_shared"), &c2).unwrap();
        assert_eq!(out["action"], json!("rebind"));
        assert_eq!(out["previous"]["member_id"], json!("m-worker"));

        let active = bindings_for_team(&conn, "t1").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0]["member_id"], json!("m-owner"));
        assert_eq!(resolve_member(&mut conn, "feishu", "ou_shared", "t1").unwrap().unwrap()["member_id"], json!("m-owner"));
    }

    #[test]
    fn a_member_keeps_one_im_identity_per_platform_and_team() {
        let (mut conn, _) = seed();
        let c1 = create_code(&mut conn, "m-worker", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        redeem(&mut conn, &ctx("ou_old"), &c1).unwrap();
        let c2 = create_code(&mut conn, "m-worker", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        let out = redeem(&mut conn, &ctx("ou_new"), &c2).unwrap();
        assert_eq!(out["previous"]["im_user_id"], json!("ou_old"));

        let mine = bindings_for_member(&conn, "m-worker", Some("t1")).unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0]["im_user_id"], json!("ou_new"));
        assert!(resolve_member(&mut conn, "feishu", "ou_old", "t1").unwrap().is_none());
    }

    #[test]
    fn unbind_is_self_service_or_lead_only() {
        let (mut conn, _) = seed();
        // a third, non-lead member of the same team
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, is_lead, joined_at)
             VALUES ('m-other','t1','s-other','Other','contributor','active',0,'now')",
            [],
        )
        .unwrap();
        let code = create_code(&mut conn, "m-worker", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        redeem(&mut conn, &ctx("ou_worker"), &code).unwrap();

        // a peer may not unbind someone else's identity
        let err = unbind(&mut conn, "m-other", "t1", "feishu", "ou_worker").unwrap_err();
        assert!(err.contains("forbidden"), "{err}");

        // self-service
        let out = unbind(&mut conn, "m-worker", "t1", "feishu", "ou_worker").unwrap();
        assert_eq!(out["unbound"], json!(true));
        assert!(resolve_member(&mut conn, "feishu", "ou_worker", "t1").unwrap().is_none());
        let actions: Vec<String> = binding_events(&conn)
            .iter()
            .map(|e| e["action"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(actions, vec!["bind".to_string(), "unbind".to_string()]);
        // unbinding twice is an error, not a silent no-op
        assert!(unbind(&mut conn, "m-worker", "t1", "feishu", "ou_worker")
            .unwrap_err()
            .contains("no active binding"));
    }

    #[test]
    fn a_lead_may_unbind_another_member() {
        let (mut conn, _) = seed();
        let code = create_code(&mut conn, "m-worker", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        redeem(&mut conn, &ctx("ou_worker"), &code).unwrap();
        let out = unbind(&mut conn, "m-owner", "t1", "feishu", "ou_worker").unwrap();
        assert_eq!(out["member_id"], json!("m-worker"));
        assert_eq!(bindings_for_team(&conn, "t1").unwrap().len(), 0);
    }

    #[test]
    fn binding_goes_invalid_when_the_member_leaves() {
        let (mut conn, _) = seed();
        let code = create_code(&mut conn, "m-worker", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        redeem(&mut conn, &ctx("ou_worker"), &code).unwrap();
        conn.execute("UPDATE members SET state = 'left' WHERE id = 'm-worker'", [])
            .unwrap();
        // identity no longer resolves, so an inbound action is answered unbound
        assert!(resolve_member(&mut conn, "feishu", "ou_worker", "t1").unwrap().is_none());
        // and a code for a departed member cannot be redeemed
        let code2 = {
            conn.execute("UPDATE members SET state = 'active' WHERE id = 'm-worker'", [])
                .unwrap();
            let c = create_code(&mut conn, "m-worker", "t1", None).unwrap()["code"]
                .as_str()
                .unwrap()
                .to_string();
            conn.execute("UPDATE members SET state = 'left' WHERE id = 'm-worker'", [])
                .unwrap();
            c
        };
        let err = redeem(&mut conn, &ctx("ou_second"), &code2).unwrap_err();
        assert!(err.contains("member_inactive"), "{err}");
    }

    #[test]
    fn listing_is_self_scoped_unless_the_caller_is_a_lead() {
        let (mut conn, _) = seed();
        for (member, im) in [("m-worker", "ou_worker"), ("m-owner", "ou_owner")] {
            let code = create_code(&mut conn, member, "t1", None).unwrap()["code"]
                .as_str()
                .unwrap()
                .to_string();
            redeem(&mut conn, &ctx(im), &code).unwrap();
        }
        let mine = list_bindings(&conn, "m-worker", "t1", false).unwrap();
        assert_eq!(mine["count"], json!(1));
        assert_eq!(mine["bindings"][0]["im_user_id"], json!("ou_worker"));

        let err = list_bindings(&conn, "m-worker", "t1", true).unwrap_err();
        assert!(err.contains("co-lead"), "{err}");

        let team = list_bindings(&conn, "m-owner", "t1", true).unwrap();
        assert_eq!(team["scope"], json!("team"));
        assert_eq!(team["count"], json!(2));
    }

    #[test]
    fn codes_are_never_stored_in_plaintext() {
        let (mut conn, _) = seed();
        let code = create_code(&mut conn, "m-worker", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        let stored: String = conn
            .query_row("SELECT code_hash FROM im_bind_codes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, hash_code(&code));
        assert!(!stored.contains(&code));
        assert_eq!(stored.len(), 64, "sha-256 hex");
    }

    #[test]
    fn rejections_are_audited_against_the_actors_team() {
        let (mut conn, _) = seed();
        // the code is unknown, but the actor's team is known → audited there
        let err = redeem_in_team(&mut conn, &ctx("ou_x"), "AAAAAAAAAA", "t1").unwrap_err();
        assert!(err.contains("code_unknown"), "{err}");
        let (n, payload): (i64, String) = {
            let mut stmt = conn
                .prepare("SELECT COUNT(*), MAX(payload_json) FROM events WHERE type = 'im.rejected'")
                .unwrap();
            stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
        };
        assert_eq!(n, 1);
        let p: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(p["detail"], json!("code_unknown"));
        assert_eq!(p["source"], json!("im:feishu"));
        // without a team context the rejection cannot be attributed
        let mut conn2 = test_conn();
        conn2
            .execute(
                "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
                 VALUES ('t9','T9',NULL,NULL,'active','tok','now','now')",
                [],
            )
            .unwrap();
        assert!(redeem(&mut conn2, &ctx("ou_y"), "AAAAAAAAAA").unwrap_err().contains("code_unknown"));
        let n2: i64 = conn2
            .query_row("SELECT COUNT(*) FROM events WHERE type = 'im.rejected'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n2, 0);
    }

    #[test]
    fn attempt_rows_are_pruned() {
        let (mut conn, _) = seed();
        redeem(&mut conn, &ctx("ou_x"), "AAAAAAAAAA").unwrap_err();
        conn.execute(
            "UPDATE im_bind_attempts SET attempted_at = ?1",
            params![chrono::Utc::now().timestamp() - ATTEMPT_RETENTION_SECS - 10],
        )
        .unwrap();
        // the next attempt prunes the stale row instead of counting it
        redeem(&mut conn, &ctx("ou_x"), "AAAAAAAAAA").unwrap_err();
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM im_bind_attempts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 1);
        assert_eq!(recent_failures(&conn, "feishu", "ou_x", RATE_LIMIT_WINDOW_SECS).unwrap(), 1);
    }
}
