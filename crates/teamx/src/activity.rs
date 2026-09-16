//! teamx enterprise — member activity analytics.
//!
//! Collects per-node member activity (time, duration, tokens, cost, work
//! content) and stores it in the team lead's serve database (`activity` table).
//! Every row records its source node (`node_id` = instance_id,
//! `node_name` = hostname). Sensitive fields (tool/command arguments, user
//! message text) are recorded in full for audit purposes.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// A single activity row, as sent by a member plugin (V1 push) or queried.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ActivityRow {
    /// Team the activity belongs to. Omitted by the plugin in network mode
    /// (serve fills it from the member's single team) and in V1 local mode
    /// (CLI fills it from the session's team). Required when ambiguous.
    #[serde(default)]
    pub team_id: String,
    /// Member the activity is attributed to. Omitted by the plugin; the serve
    /// RPC and the V1 local CLI both resolve it from the authenticated
    /// identity / session key.
    #[serde(default)]
    pub member_id: String,
    pub node_id: String,
    #[serde(default)]
    pub node_name: Option<String>,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    pub kind: String,
    /// JSON detail (tool/command args, user message text). Accepts an object
    /// (serialized on insert) or a pre-serialized JSON string.
    #[serde(default)]
    pub detail: Option<Value>,
    #[serde(default)]
    pub tokens_input: Option<i64>,
    #[serde(default)]
    pub tokens_output: Option<i64>,
    #[serde(default)]
    pub tokens_reasoning: Option<i64>,
    #[serde(default)]
    pub cost: Option<f64>,
    /// Model attribution (pricing-table key); provider first, model second.
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub provider_id: Option<String>,
    /// Cache token counts, lifted out of `detail` so cost can be computed
    /// server-side from columns.
    #[serde(default)]
    pub tokens_cache_read: Option<i64>,
    #[serde(default)]
    pub tokens_cache_write: Option<i64>,
    /// Cost provenance: opencode | computed | none.
    #[serde(default)]
    pub cost_source: Option<String>,
    /// Cost decomposition (USD): input / output / cache-read / cache-write.
    #[serde(default)]
    pub cost_input: Option<f64>,
    #[serde(default)]
    pub cost_output: Option<f64>,
    #[serde(default)]
    pub cost_cache_read: Option<f64>,
    #[serde(default)]
    pub cost_cache_write: Option<f64>,
    #[serde(default)]
    pub has_human: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
}

impl ActivityRow {
    /// SQL for inserting one row (created_at set here, not by the caller).
    pub const INSERT: &'static str = "\
INSERT INTO activity
  (team_id, member_id, node_id, node_name, started_at, ended_at, duration_ms,
   kind, detail, tokens_input, tokens_output, tokens_reasoning, cost,
   model_id, provider_id, tokens_cache_read, tokens_cache_write,
   cost_source, cost_input, cost_output, cost_cache_read, cost_cache_write,
   has_human, created_at)
VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)";
}

/// Push a batch of activity rows into the `activity` table.
///
/// Authorization (enforced by the caller, e.g. serve RPC dispatch):
///  - a member may only write rows for their own member_id
///  - node_id is taken from the row, but the server may override it from the
///    caller's own instance when the row omits it.
pub fn push_activities(conn: &mut Connection, rows: &[ActivityRow]) -> rusqlite::Result<usize> {
    let mut inserted = 0usize;
    crate::db::with_write(conn, |tx| {
        {
            let mut stmt = tx.prepare(ActivityRow::INSERT)?;
            for row in rows {
                let now = crate::db::now();
                let detail = row.detail.as_ref().map(|v| v.to_string());
                let has_human = row.has_human.map(|h| if h { 1i64 } else { 0i64 }).unwrap_or(0);
                let cost_source = row.cost_source.as_deref().unwrap_or("none");
                stmt.execute(params![
                    row.team_id,
                    row.member_id,
                    row.node_id,
                    row.node_name,
                    row.started_at,
                    row.ended_at,
                    row.duration_ms,
                    row.kind,
                    detail,
                    row.tokens_input,
                    row.tokens_output,
                    row.tokens_reasoning,
                    row.cost,
                    row.model_id,
                    row.provider_id,
                    row.tokens_cache_read,
                    row.tokens_cache_write,
                    cost_source,
                    row.cost_input,
                    row.cost_output,
                    row.cost_cache_read,
                    row.cost_cache_write,
                    has_human,
                    now,
                ])?;
                inserted += 1;
            }
        }
        Ok(())
    })?;
    Ok(inserted)
}

// ---------------------------------------------------------------------------
// Budget alert: env-configured cumulative-cost threshold, crossing-triggered.
// ---------------------------------------------------------------------------

/// Ledger event emitted when a member's cumulative cost crosses the budget.
pub const BUDGET_ALERT_EVENT: &str = "budget.alert";

/// Env var holding the per-member per-team cumulative cost threshold (USD).
pub const BUDGET_ALERT_ENV: &str = "TEAMX_BUDGET_ALERT_COST_USD";

/// Parse a threshold value: must be finite and > 0, otherwise disabled.
pub fn parse_budget_threshold(raw: &str) -> Option<f64> {
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|t| t.is_finite() && *t > 0.0)
}

/// Threshold from `TEAMX_BUDGET_ALERT_COST_USD`; unset / invalid / <= 0
/// disables alerting entirely.
pub fn budget_threshold_from_env() -> Option<f64> {
    std::env::var(BUDGET_ALERT_ENV)
        .ok()
        .and_then(|s| parse_budget_threshold(&s))
}

/// Cumulative recorded cost (USD) for one member within one team.
pub fn member_cost_sum(conn: &Connection, team_id: &str, member_id: &str) -> rusqlite::Result<f64> {
    conn.query_row(
        "SELECT COALESCE(SUM(cost), 0.0) FROM activity WHERE team_id = ?1 AND member_id = ?2",
        params![team_id, member_id],
        |r| r.get(0),
    )
}

/// Emit one `budget.alert` ledger event iff the cumulative cost crosses the
/// threshold (`before < threshold <= after`). Cost is monotonic per member,
/// so a given threshold alerts at most once. The event lands in the ledger
/// and is fanned out to live WS subscribers by the RPC layer like any other
/// event; admin-console presentation is deliberately out of scope here.
///
/// Returns `Ok(true)` when an event was emitted.
pub fn budget_alert_on_crossing(
    conn: &mut Connection,
    team_id: &str,
    member_id: &str,
    before: f64,
    after: f64,
    threshold: f64,
) -> rusqlite::Result<bool> {
    if !(before < threshold && threshold <= after) {
        return Ok(false);
    }
    let payload = json!({
        "team_id": team_id,
        "member_id": member_id,
        "period": "cumulative",
        "usage_cost_usd": after,
        "threshold_usd": threshold,
    });
    crate::db::with_write(conn, |tx| {
        crate::events::emit(tx, team_id, Some(member_id), BUDGET_ALERT_EVENT, Some(&payload))
    })?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Enterprise cost: per-(provider, model) pricing + token → cost fallback.
// ---------------------------------------------------------------------------

/// Per-(provider, model) token pricing in USD per 1M tokens.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModelPrice {
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub model_id: String,
    pub price_input: f64,
    pub price_output: f64,
    #[serde(default)]
    pub price_cache_read: f64,
    #[serde(default)]
    pub price_cache_write: f64,
    /// >200k-context tier (all four must be present to activate).
    #[serde(default)]
    pub over200k_input: Option<f64>,
    #[serde(default)]
    pub over200k_output: Option<f64>,
    #[serde(default)]
    pub over200k_cache_read: Option<f64>,
    #[serde(default)]
    pub over200k_cache_write: Option<f64>,
}

/// A step's cost broken into input / output / cache-read / cache-write (USD).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct CostBreakdown {
    pub cost_input: f64,
    pub cost_output: f64,
    pub cost_cache_read: f64,
    pub cost_cache_write: f64,
    pub total: f64,
}

impl ModelPrice {
    /// Compute a step's cost breakdown from token counts. Returns `None` when
    /// there is no token data to price (all counts zero/absent).
    ///
    /// `tokens_input` already includes cache-read tokens (opencode accounting),
    /// so non-cached input is billed at `price_input` and cache-read at
    /// `price_cache_read`. `reasoning` is assumed folded into `output` (§4.4).
    pub fn compute(
        &self,
        tokens_input: Option<i64>,
        tokens_output: Option<i64>,
        _tokens_reasoning: Option<i64>,
        cache_read: Option<i64>,
        cache_write: Option<i64>,
    ) -> Option<CostBreakdown> {
        let ti = tokens_input.unwrap_or(0) as f64;
        let to = tokens_output.unwrap_or(0) as f64;
        let cr = cache_read.unwrap_or(0) as f64;
        let cw = cache_write.unwrap_or(0) as f64;
        if ti == 0.0 && to == 0.0 && cr == 0.0 && cw == 0.0 {
            return None;
        }
        // >200k context tier: only when input exceeds 200k and the tier exists.
        let over = ti > 200_000.0 && self.over200k_input.is_some();
        let (pi, po, pcr, pcw) = if over {
            (
                self.over200k_input.unwrap_or(self.price_input),
                self.over200k_output.unwrap_or(self.price_output),
                self.over200k_cache_read.unwrap_or(self.price_cache_read),
                self.over200k_cache_write.unwrap_or(self.price_cache_write),
            )
        } else {
            (
                self.price_input,
                self.price_output,
                self.price_cache_read,
                self.price_cache_write,
            )
        };
        let non_cached_input = (ti - cr).max(0.0);
        let cost_input = non_cached_input / 1_000_000.0 * pi;
        let cost_cache_read = cr / 1_000_000.0 * pcr;
        let cost_cache_write = cw / 1_000_000.0 * pcw;
        let cost_output = to / 1_000_000.0 * po;
        let total = cost_input + cost_output + cost_cache_read + cost_cache_write;
        Some(CostBreakdown {
            cost_input,
            cost_output,
            cost_cache_read,
            cost_cache_write,
            total,
        })
    }
}

/// Load all model prices from `model_pricing`, keyed by (provider_id, model_id).
pub fn load_prices(conn: &Connection) -> rusqlite::Result<HashMap<(String, String), ModelPrice>> {
    let mut stmt = conn.prepare(
        "SELECT provider_id, model_id, price_input, price_output, price_cache_read, price_cache_write,
                over200k_input, over200k_output, over200k_cache_read, over200k_cache_write
         FROM model_pricing",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ModelPrice {
            provider_id: r.get(0)?,
            model_id: r.get(1)?,
            price_input: r.get(2)?,
            price_output: r.get(3)?,
            price_cache_read: r.get(4)?,
            price_cache_write: r.get(5)?,
            over200k_input: r.get(6)?,
            over200k_output: r.get(7)?,
            over200k_cache_read: r.get(8)?,
            over200k_cache_write: r.get(9)?,
        })
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let p = row?;
        map.insert((p.provider_id.clone(), p.model_id.clone()), p);
    }
    Ok(map)
}

/// Resolve cost for a batch of activity rows against the pricing table.
///
/// For `step_finish` / `work_session` rows lacking an opencode cost, compute
/// cost from tokens × pricing (`cost_source = 'computed'`). Rows that already
/// carry an opencode cost keep it authoritative (`cost_source = 'opencode'`)
/// but still get their decomposition filled from pricing for display. Other
/// rows are marked `'none'`.
pub fn apply_pricing(conn: &Connection, rows: &mut [ActivityRow]) -> rusqlite::Result<()> {
    let prices = load_prices(conn)?;
    for row in rows {
        if row.kind != "step_finish" && row.kind != "work_session" {
            // Non-step/work rows never carry cost; mark provenance for clarity.
            row.cost_source = Some("none".to_string());
            continue;
        }
        let price = row
            .provider_id
            .as_deref()
            .zip(row.model_id.as_deref())
            .and_then(|(p, m)| prices.get(&(p.to_string(), m.to_string())));
        let breakdown = price.and_then(|p| {
            p.compute(
                row.tokens_input,
                row.tokens_output,
                row.tokens_reasoning,
                row.tokens_cache_read,
                row.tokens_cache_write,
            )
        });
        let has_opencode_cost = row.cost.is_some_and(|c| c > 0.0);
        if has_opencode_cost {
            row.cost_source = Some("opencode".to_string());
        } else if let Some(b) = &breakdown {
            row.cost = Some(b.total);
            row.cost_source = Some("computed".to_string());
        } else {
            row.cost_source = Some("none".to_string());
        }
        if let Some(b) = breakdown {
            row.cost_input = Some(b.cost_input);
            row.cost_output = Some(b.cost_output);
            row.cost_cache_read = Some(b.cost_cache_read);
            row.cost_cache_write = Some(b.cost_cache_write);
        }
    }
    Ok(())
}

/// Upsert pricing rows. `source` is `"opencode"` or `"owner"`.
/// `owner` always overwrites; `opencode` never overwrites an existing `owner`
/// row (priority: owner > opencode > builtin).
pub fn upsert_prices(conn: &Connection, prices: &[ModelPrice], source: &str) -> rusqlite::Result<usize> {
    let guard = if source == "owner" {
        ""
    } else {
        "WHERE model_pricing.source != 'owner'"
    };
    let now = crate::db::now();
    let sql = format!(
        "INSERT INTO model_pricing
           (provider_id, model_id, price_input, price_output, price_cache_read, price_cache_write,
            over200k_input, over200k_output, over200k_cache_read, over200k_cache_write, source, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(provider_id, model_id) DO UPDATE SET
           price_input=excluded.price_input, price_output=excluded.price_output,
           price_cache_read=excluded.price_cache_read, price_cache_write=excluded.price_cache_write,
           over200k_input=excluded.over200k_input, over200k_output=excluded.over200k_output,
           over200k_cache_read=excluded.over200k_cache_read, over200k_cache_write=excluded.over200k_cache_write,
           source=excluded.source, updated_at=excluded.updated_at
         {guard}"
    );
    let mut inserted = 0usize;
    for p in prices {
        inserted += conn.execute(
            &sql,
            params![
                p.provider_id,
                p.model_id,
                p.price_input,
                p.price_output,
                p.price_cache_read,
                p.price_cache_write,
                p.over200k_input,
                p.over200k_output,
                p.over200k_cache_read,
                p.over200k_cache_write,
                source,
                now,
            ],
        )?;
    }
    Ok(inserted)
}

/// Built-in pricing seed (lowest priority, `INSERT OR IGNORE` so it never
/// overwrites). Approximate public list prices in USD / 1M tokens; overwritten
/// by opencode-synced or owner-provided rows.
pub fn seed_builtin_pricing(conn: &Connection) -> rusqlite::Result<usize> {
    // (provider, model, input, output, cache_read, cache_write, over200k)
    type SeedEntry = (&'static str, &'static str, f64, f64, f64, f64, Option<(f64, f64, f64, f64)>);
    const SEED: &[SeedEntry] = &[
        ("anthropic", "claude-sonnet-4-5", 3.0, 15.0, 0.30, 3.75, Some((6.0, 30.0, 0.60, 7.50))),
        ("anthropic", "claude-opus-4-1", 15.0, 75.0, 1.50, 18.75, Some((30.0, 150.0, 3.0, 37.5))),
        ("anthropic", "claude-haiku-4-5", 1.0, 5.0, 0.10, 1.25, None),
        ("openai", "gpt-4o", 2.5, 10.0, 1.25, 2.5, None),
        ("openai", "gpt-4o-mini", 0.15, 0.60, 0.075, 0.15, None),
        ("openai", "gpt-4.1", 2.0, 8.0, 0.5, 2.0, None),
        ("openai", "gpt-4.1-mini", 0.40, 1.60, 0.10, 0.40, None),
        ("deepseek", "deepseek-chat", 0.27, 1.10, 0.0, 0.0, None),
        ("deepseek", "deepseek-reasoner", 0.55, 2.19, 0.0, 0.0, None),
    ];
    let mut inserted = 0usize;
    for &(provider, model, pi, po, pcr, pcw, over) in SEED {
        let now = crate::db::now();
        let (oi, oo, ocr, ocw) = match over {
            Some((a, b, c, d)) => (Some(a), Some(b), Some(c), Some(d)),
            None => (None, None, None, None),
        };
        inserted += conn.execute(
            "INSERT OR IGNORE INTO model_pricing
               (provider_id, model_id, price_input, price_output, price_cache_read, price_cache_write,
                over200k_input, over200k_output, over200k_cache_read, over200k_cache_write, source, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'builtin',?11)",
            params![provider, model, pi, po, pcr, pcw, oi, oo, ocr, ocw, now],
        )?;
    }
    Ok(inserted)
}

// ---------------------------------------------------------------------------
// Enterprise presence: per-member real-time state (active sessions / running
// tools / open terminals). Latest snapshot only; a TTL marks stale rows stale.
// ---------------------------------------------------------------------------

/// Presence rows older than this are treated as offline (member heartbeat
/// stopped arriving).
pub const PRESENCE_TTL_MS: i64 = 60_000;

/// A member's live presence snapshot, pushed by the plugin.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Presence {
    #[serde(default)]
    pub active_sessions: i64,
    #[serde(default)]
    pub running_tools: i64,
    #[serde(default)]
    pub open_terminals: i64,
}

/// Upsert a member's presence snapshot (latest only).
pub fn upsert_presence(conn: &Connection, member_id: &str, p: &Presence) -> rusqlite::Result<()> {
    let now = crate::db::now();
    conn.execute(
        "INSERT INTO presence (member_id, active_sessions, running_tools, open_terminals, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(member_id) DO UPDATE SET
           active_sessions = excluded.active_sessions,
           running_tools   = excluded.running_tools,
           open_terminals  = excluded.open_terminals,
           updated_at      = excluded.updated_at",
        params![
            member_id,
            p.active_sessions,
            p.running_tools,
            p.open_terminals,
            now
        ],
    )?;
    Ok(())
}

/// Load live presence for all members, dropping rows older than the TTL.
pub fn presence_for_members(conn: &Connection) -> rusqlite::Result<HashMap<String, Presence>> {
    let now = chrono::Utc::now();
    let mut out = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT member_id, active_sessions, running_tools, open_terminals, updated_at FROM presence",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (member_id, a, t, o, updated_at) = row?;
        let fresh = chrono::DateTime::parse_from_rfc3339(&updated_at)
            .ok()
            .map(|ts| (now - ts.with_timezone(&chrono::Utc)).num_milliseconds() < PRESENCE_TTL_MS)
            .unwrap_or(false);
        if fresh {
            out.insert(
                member_id,
                Presence {
                    active_sessions: a,
                    running_tools: t,
                    open_terminals: o,
                },
            );
        }
    }
    Ok(out)
}

/// Rolling window (ms) for the Members-view "recent activity" cards. This is
/// long enough to smooth out the transient "currently running tool" signal and
/// reflect a member's sustained activity instead.
pub const RECENT_WINDOW_MS: i64 = 600_000; // 10 minutes

/// Aggregates over the last `window_ms` of activity, for the Members-view cards:
/// distinct active members, work sessions, tool calls, human actions, and total
/// rows. A member is "active" when it has any activity in the window.
pub fn recent_activity(conn: &Connection, team: Option<&str>, window_ms: i64) -> rusqlite::Result<Value> {
    let since = (chrono::Utc::now() - chrono::Duration::milliseconds(window_ms)).to_rfc3339();
    let f = filter(team, None, None, None, Some(&since), None);
    let q = format!(
        "SELECT COUNT(DISTINCT member_id), \
                COALESCE(SUM(CASE WHEN kind = 'work_session' THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN kind = 'tool_call' THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN kind IN ('human_input','human_approval','human_command') THEN 1 ELSE 0 END), 0), \
                COUNT(*) \
         FROM activity WHERE {}",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    stmt.query_row(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "active_members": r.get::<_, i64>(0)?,
            "work_sessions": r.get::<_, i64>(1)?,
            "tool_calls": r.get::<_, i64>(2)?,
            "human_actions": r.get::<_, i64>(3)?,
            "total_rows": r.get::<_, i64>(4)?,
        }))
    })
}

/// Build a `WHERE` fragment (with bindable params) from optional filters.
/// Supported filters: team, member, node, kind, from (>=, inclusive), to (<, exclusive).
/// Always constrains to a team when `team` is present.
struct Filter {
    sql: String,
    params: Vec<Value>,
}

fn filter(team: Option<&str>, member: Option<&str>, node: Option<&str>, kind: Option<&str>, from: Option<&str>, to: Option<&str>) -> Filter {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(t) = team {
        if !t.is_empty() {
            clauses.push("team_id = ?".to_string());
            params.push(json!(t));
        }
    }
    if let Some(m) = member {
        if !m.is_empty() {
            clauses.push("member_id = ?".to_string());
            params.push(json!(m));
        }
    }
    if let Some(n) = node {
        if !n.is_empty() {
            clauses.push("node_id = ?".to_string());
            params.push(json!(n));
        }
    }
    if let Some(k) = kind {
        clauses.push("kind = ?".to_string());
        params.push(json!(k));
    }
    if let Some(f) = from {
        clauses.push("started_at >= ?".to_string());
        params.push(json!(f));
    }
    if let Some(t) = to {
        clauses.push("started_at < ?".to_string());
        params.push(json!(t));
    }
    let sql = if clauses.is_empty() {
        "1=1".to_string()
    } else {
        clauses.join(" AND ")
    };
    Filter { sql, params }
}

/// Aggregate overview for a team + filters: totals for duration/tokens/cost,
/// active member/node counts, and human-vs-ai distribution.
pub fn summary(conn: &Connection, team: Option<&str>, member: Option<&str>, node: Option<&str>, kind: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(team, member, node, kind, from, to);
    // One-row aggregate helper: returns {duration_ms,cost,tokens_*,cost_*,count}.
    let aggregate = |extra_clause: &str| -> rusqlite::Result<Value> {
        let q = format!(
            "SELECT SUM(duration_ms), SUM(cost), SUM(tokens_input), SUM(tokens_output), SUM(tokens_reasoning), \
                    SUM(cost_input), SUM(cost_output), SUM(cost_cache_read), SUM(cost_cache_write), COUNT(*) \
             FROM activity WHERE {} {extra_clause}",
            f.sql
        );
        let mut stmt = conn.prepare(&q)?;
        let row = stmt.query_row(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
            Ok(json!({
                "duration_ms": r.get::<_, Option<i64>>(0)?,
                "cost": r.get::<_, Option<f64>>(1)?,
                "tokens_input": r.get::<_, Option<i64>>(2)?,
                "tokens_output": r.get::<_, Option<i64>>(3)?,
                "tokens_reasoning": r.get::<_, Option<i64>>(4)?,
                "cost_input": r.get::<_, Option<f64>>(5)?,
                "cost_output": r.get::<_, Option<f64>>(6)?,
                "cost_cache_read": r.get::<_, Option<f64>>(7)?,
                "cost_cache_write": r.get::<_, Option<f64>>(8)?,
                "count": r.get::<_, i64>(9)?,
            }))
        })?;
        Ok(row)
    };
    let overall = aggregate("")?;
    let human = aggregate(
        "AND kind IN ('human_input','human_approval','human_command')",
    )?;
    let ai = aggregate(
        "AND kind NOT IN ('human_input','human_approval','human_command')",
    )?;
    // Distinct active members / nodes.
    let count_distinct = |col: &str| -> rusqlite::Result<i64> {
        let q = format!("SELECT COUNT(DISTINCT {col}) FROM activity WHERE {}", f.sql);
        let mut stmt = conn.prepare(&q)?;
        stmt.query_row(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| r.get(0))
    };
    let members = count_distinct("member_id")?;
    let nodes = count_distinct("node_id")?;
    // Flat mirrors of `overall` (the /api/overview contract exposes duration/
    // tokens/cost at the top level of the inner `overview` object).
    let (duration_ms, cost, tokens_input, tokens_output, tokens_reasoning) = (
        overall["duration_ms"].clone(),
        overall["cost"].clone(),
        overall["tokens_input"].clone(),
        overall["tokens_output"].clone(),
        overall["tokens_reasoning"].clone(),
    );
    let (cost_input, cost_output, cost_cache_read, cost_cache_write, count) = (
        overall["cost_input"].clone(),
        overall["cost_output"].clone(),
        overall["cost_cache_read"].clone(),
        overall["cost_cache_write"].clone(),
        overall["count"].clone(),
    );
    Ok(json!({
        "team_id": team.unwrap_or_default(),
        "overall": overall,
        "human": human,
        "ai": ai,
        "active_members": members,
        "active_nodes": nodes,
        "duration_ms": duration_ms,
        "cost": cost,
        "tokens_input": tokens_input,
        "tokens_output": tokens_output,
        "tokens_reasoning": tokens_reasoning,
        "cost_input": cost_input,
        "cost_output": cost_output,
        "cost_cache_read": cost_cache_read,
        "cost_cache_write": cost_cache_write,
        "count": count,
    }))
}

/// Per-member aggregates (duration/tokens/cost + counts), for a team.
pub fn by_member(conn: &Connection, team: Option<&str>, member: Option<&str>, node: Option<&str>, kind: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(team, member, node, kind, from, to);
    let q = format!(
        "SELECT member_id, MAX(node_name), SUM(duration_ms), SUM(cost), SUM(tokens_input), SUM(tokens_output), SUM(tokens_reasoning), \
                SUM(cost_input), SUM(cost_output), SUM(cost_cache_read), SUM(cost_cache_write), COUNT(*) \
         FROM activity WHERE {} GROUP BY member_id ORDER BY COUNT(*) DESC",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "member_id": r.get::<_, String>(0)?,
            "node_name": r.get::<_, Option<String>>(1)?,
            "duration_ms": r.get::<_, Option<i64>>(2)?,
            "cost": r.get::<_, Option<f64>>(3)?,
            "tokens_input": r.get::<_, Option<i64>>(4)?,
            "tokens_output": r.get::<_, Option<i64>>(5)?,
            "tokens_reasoning": r.get::<_, Option<i64>>(6)?,
            "cost_input": r.get::<_, Option<f64>>(7)?,
            "cost_output": r.get::<_, Option<f64>>(8)?,
            "cost_cache_read": r.get::<_, Option<f64>>(9)?,
            "cost_cache_write": r.get::<_, Option<f64>>(10)?,
            "count": r.get::<_, i64>(11)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(json!(out))
}

/// Per-node aggregates for a team.
pub fn by_node(conn: &Connection, team: Option<&str>, member: Option<&str>, node: Option<&str>, kind: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(team, member, node, kind, from, to);
    let q = format!(
        "SELECT node_id, MAX(node_name), SUM(duration_ms), SUM(cost), SUM(tokens_input), SUM(tokens_output), SUM(tokens_reasoning), \
                SUM(cost_input), SUM(cost_output), SUM(cost_cache_read), SUM(cost_cache_write), COUNT(*) \
         FROM activity WHERE {} GROUP BY node_id ORDER BY COUNT(*) DESC",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "node_id": r.get::<_, String>(0)?,
            "node_name": r.get::<_, Option<String>>(1)?,
            "duration_ms": r.get::<_, Option<i64>>(2)?,
            "cost": r.get::<_, Option<f64>>(3)?,
            "tokens_input": r.get::<_, Option<i64>>(4)?,
            "tokens_output": r.get::<_, Option<i64>>(5)?,
            "tokens_reasoning": r.get::<_, Option<i64>>(6)?,
            "cost_input": r.get::<_, Option<f64>>(7)?,
            "cost_output": r.get::<_, Option<f64>>(8)?,
            "cost_cache_read": r.get::<_, Option<f64>>(9)?,
            "cost_cache_write": r.get::<_, Option<f64>>(10)?,
            "count": r.get::<_, i64>(11)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(json!(out))
}

/// Tool-call distribution (top N by count), for a team. Reads `kind = 'tool_call'`
/// rows and counts distinct tool names from the JSON detail.
pub fn tools(conn: &Connection, team: Option<&str>, member: Option<&str>, node: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(team, member, node, Some("tool_call"), from, to);
    let q = format!("SELECT detail FROM activity WHERE {}", f.sql);
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| r.get::<_, String>(0))?;
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for row in rows {
        let detail = row?;
        if let Ok(v) = serde_json::from_str::<Value>(&detail) {
            let name = v.get("tool").and_then(Value::as_str).unwrap_or("?");
            *counts.entry(name.to_string()).or_insert(0) += 1;
        }
    }
    let mut out: Vec<Value> = counts
        .into_iter()
        .map(|(tool, count)| json!({ "tool": tool, "count": count }))
        .collect();
    out.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
    Ok(json!(out))
}

/// File-edit distribution (top N by count), for a team. Reads `kind = 'file_edit'`
/// rows and counts distinct file paths from the JSON detail.
pub fn files(conn: &Connection, team: Option<&str>, member: Option<&str>, node: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(team, member, node, Some("file_edit"), from, to);
    let q = format!("SELECT detail FROM activity WHERE {}", f.sql);
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| r.get::<_, String>(0))?;
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for row in rows {
        let detail = row?;
        if let Ok(v) = serde_json::from_str::<Value>(&detail) {
            let file = v.get("file").and_then(Value::as_str).unwrap_or("?");
            *counts.entry(file.to_string()).or_insert(0) += 1;
        }
    }
    let mut out: Vec<Value> = counts
        .into_iter()
        .map(|(file, count)| json!({ "file": file, "count": count }))
        .collect();
    out.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
    Ok(json!(out))
}

/// Timeline / Gantt data for a team: work segments (work_session rows with
/// started_at + ended_at + duration_ms) plus point-in-time events (tool_call,
/// step_finish, command, file_edit, human_*) — all grouped per member, ordered
/// by start time. Used by the `Timeline` (Gantt) view of the dashboard.
pub fn timeline(conn: &Connection, team: Option<&str>, member: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(team, member, None, None, from, to);
    let q = format!(
        "SELECT member_id, node_id, node_name, kind, started_at, ended_at, duration_ms, detail, tokens_input, tokens_output, tokens_reasoning, cost, has_human, model_id, provider_id, tokens_cache_read, tokens_cache_write, cost_source, cost_input, cost_output, cost_cache_read, cost_cache_write \
         FROM activity WHERE {} ORDER BY started_at ASC",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "member_id": r.get::<_, String>(0)?,
            "node_id": r.get::<_, String>(1)?,
            "node_name": r.get::<_, Option<String>>(2)?,
            "kind": r.get::<_, String>(3)?,
            "started_at": r.get::<_, String>(4)?,
            "ended_at": r.get::<_, Option<String>>(5)?,
            "duration_ms": r.get::<_, Option<i64>>(6)?,
            "detail": r.get::<_, Option<String>>(7)?,
            "tokens_input": r.get::<_, Option<i64>>(8)?,
            "tokens_output": r.get::<_, Option<i64>>(9)?,
            "tokens_reasoning": r.get::<_, Option<i64>>(10)?,
            "cost": r.get::<_, Option<f64>>(11)?,
            "has_human": r.get::<_, Option<bool>>(12)?,
            "model_id": r.get::<_, Option<String>>(13)?,
            "provider_id": r.get::<_, Option<String>>(14)?,
            "tokens_cache_read": r.get::<_, Option<i64>>(15)?,
            "tokens_cache_write": r.get::<_, Option<i64>>(16)?,
            "cost_source": r.get::<_, Option<String>>(17)?,
            "cost_input": r.get::<_, Option<f64>>(18)?,
            "cost_output": r.get::<_, Option<f64>>(19)?,
            "cost_cache_read": r.get::<_, Option<f64>>(20)?,
            "cost_cache_write": r.get::<_, Option<f64>>(21)?,
        }))
    })?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(json!(items))
}

/// Detail rows (recent activity list), newest first, optional limit.
#[allow(clippy::too_many_arguments)]
pub fn rows(conn: &Connection, team: Option<&str>, member: Option<&str>, node: Option<&str>, kind: Option<&str>, from: Option<&str>, to: Option<&str>, limit: i64) -> rusqlite::Result<Value> {
    let f = filter(team, member, node, kind, from, to);
    let limit = limit.clamp(1, 1000);
    let q = format!(
        "SELECT id, team_id, member_id, node_id, node_name, started_at, ended_at, duration_ms, kind, detail, tokens_input, tokens_output, tokens_reasoning, cost, has_human, created_at \
         FROM activity WHERE {} ORDER BY id DESC LIMIT {limit}",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "team_id": r.get::<_, String>(1)?,
            "member_id": r.get::<_, String>(2)?,
            "node_id": r.get::<_, String>(3)?,
            "node_name": r.get::<_, Option<String>>(4)?,
            "started_at": r.get::<_, String>(5)?,
            "ended_at": r.get::<_, Option<String>>(6)?,
            "duration_ms": r.get::<_, Option<i64>>(7)?,
            "kind": r.get::<_, String>(8)?,
            "detail": r.get::<_, Option<String>>(9)?,
            "tokens_input": r.get::<_, Option<i64>>(10)?,
            "tokens_output": r.get::<_, Option<i64>>(11)?,
            "tokens_reasoning": r.get::<_, Option<i64>>(12)?,
            "cost": r.get::<_, Option<f64>>(13)?,
            "has_human": r.get::<_, Option<bool>>(14)?,
            "created_at": r.get::<_, Option<String>>(15)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(json!(out))
}

/// Paged activity rows, newest first, with `has_more`. The `detail` JSON is
/// parsed into an object so the UI can show tool args/output directly.
#[allow(clippy::too_many_arguments)]
pub fn rows_page(
    conn: &Connection,
    team: Option<&str>,
    member: Option<&str>,
    node: Option<&str>,
    kind: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    limit: i64,
    offset: i64,
) -> rusqlite::Result<(Vec<Value>, bool)> {
    let f = filter(team, member, node, kind, from, to);
    let limit = limit.clamp(1, 500);
    let offset = offset.max(0);
    // Ask for one extra row to know whether there is a next page.
    let q = format!(
        "SELECT id, member_id, node_id, node_name, started_at, duration_ms, kind, detail, \
                tokens_input, tokens_output, cost, has_human, model_id, provider_id \
         FROM activity WHERE {} ORDER BY id DESC LIMIT {limit} OFFSET {offset}",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let mut items: Vec<Value> = stmt
        .query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
            let detail_raw: Option<String> = r.get(7)?;
            let detail = detail_raw
                .as_deref()
                .and_then(|d| serde_json::from_str(d).ok())
                .unwrap_or(Value::Null);
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "member_id": r.get::<_, String>(1)?,
                "node_id": r.get::<_, String>(2)?,
                "node_name": r.get::<_, Option<String>>(3)?,
                "started_at": r.get::<_, String>(4)?,
                "duration_ms": r.get::<_, Option<i64>>(5)?,
                "kind": r.get::<_, String>(6)?,
                "detail": detail,
                "tokens_input": r.get::<_, Option<i64>>(8)?,
                "tokens_output": r.get::<_, Option<i64>>(9)?,
                "cost": r.get::<_, Option<f64>>(10)?,
                "has_human": r.get::<_, Option<bool>>(11)?,
                "model_id": r.get::<_, Option<String>>(12)?,
                "provider_id": r.get::<_, Option<String>>(13)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let has_more = items.len() as i64 > limit;
    if has_more {
        items.pop();
    }
    Ok((items, has_more))
}

/// Human activity (human_input / human_approval / human_command) rows, for
/// answering "what did the human actually do". Newest first.
pub fn human_rows(conn: &Connection, team: Option<&str>, member: Option<&str>, node: Option<&str>, from: Option<&str>, to: Option<&str>, limit: i64) -> rusqlite::Result<Value> {    let f = filter(team, member, node, None, from, to);
    let limit = limit.clamp(1, 1000);
    let q = format!(
        "SELECT id, member_id, node_id, node_name, started_at, kind, detail \
         FROM activity WHERE {} AND kind IN ('human_input','human_approval','human_command') \
         ORDER BY id DESC LIMIT {limit}",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "member_id": r.get::<_, String>(1)?,
            "node_id": r.get::<_, String>(2)?,
            "node_name": r.get::<_, Option<String>>(3)?,
            "started_at": r.get::<_, String>(4)?,
            "kind": r.get::<_, String>(5)?,
            "detail": r.get::<_, Option<String>>(6)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        db::migrate(&conn).unwrap();
        conn
    }

    fn seed_team(conn: &Connection, team_id: &str, member_id: &str) {
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES (?1, 'T', ?2, NULL, 'forming', 'tok', 'now', 'now')",
            params![team_id, member_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES (?1, ?2, 's', 'm', 'owner', 'active', 'now')",
            params![member_id, team_id],
        )
        .unwrap();
    }

    fn base_row(team: &str, member: &str, kind: &str) -> ActivityRow {
        ActivityRow {
            team_id: team.to_string(),
            member_id: member.to_string(),
            node_id: "node-a".to_string(),
            node_name: Some("host-a".to_string()),
            started_at: "2026-08-19T00:00:00Z".to_string(),
            ended_at: None,
            duration_ms: None,
            kind: kind.to_string(),
            detail: None,
            tokens_input: None,
            tokens_output: None,
            tokens_reasoning: None,
            cost: None,
            model_id: None,
            provider_id: None,
            tokens_cache_read: None,
            tokens_cache_write: None,
            cost_source: None,
            cost_input: None,
            cost_output: None,
            cost_cache_read: None,
            cost_cache_write: None,
            has_human: Some(false),
            created_at: None,
        }
    }

    #[test]
    fn push_and_query_summary() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let mut rows = [base_row("t1", "m1", "tool_call")];
        rows[0].detail = Some(serde_json::json!({"tool":"bash","state":"completed"}));
        rows[0].tokens_input = Some(100);
        rows[0].tokens_output = Some(50);
        rows[0].cost = Some(0.01);
        rows[0].started_at = "2026-08-19T10:00:00Z".to_string();
        let mut h = base_row("t1", "m1", "human_input");
        h.detail = Some(serde_json::json!({"sessionID":"s1","text":"fix the bug"}));
        h.started_at = "2026-08-19T11:00:00Z".to_string();
        push_activities(&mut conn, &[rows[0].clone(), h]).unwrap();

        let s = summary(&conn, Some("t1"), None, None, None, None, None).unwrap();
        assert_eq!(s["overall"]["count"], 2);
        assert_eq!(s["overall"]["tokens_input"], 100);
        assert_eq!(s["overall"]["tokens_output"], 50);
        assert_eq!(s["overall"]["cost"], 0.01);
        assert_eq!(s["human"]["count"], 1);
        assert_eq!(s["ai"]["count"], 1);
        assert_eq!(s["active_members"], 1);
        assert_eq!(s["active_nodes"], 1);
    }

    fn alert_count(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'budget.alert'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn budget_alert_fires_on_crossing_once() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let mut r = base_row("t1", "m1", "step_finish");
        r.cost = Some(0.6);
        push_activities(&mut conn, &[r]).unwrap();

        // Below threshold: no event.
        let before = member_cost_sum(&conn, "t1", "m1").unwrap();
        assert!((before - 0.6).abs() < 1e-9);
        assert!(!budget_alert_on_crossing(&mut conn, "t1", "m1", 0.0, before, 1.0).unwrap());
        assert_eq!(alert_count(&conn), 0);

        // Push the member over the threshold, then detect the crossing.
        let mut r2 = base_row("t1", "m1", "step_finish");
        r2.cost = Some(0.5);
        push_activities(&mut conn, &[r2]).unwrap();
        let after = member_cost_sum(&conn, "t1", "m1").unwrap();
        assert!(budget_alert_on_crossing(&mut conn, "t1", "m1", before, after, 1.0).unwrap());
        assert_eq!(alert_count(&conn), 1);

        // Payload carries member / period / usage / threshold.
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM events WHERE type = 'budget.alert'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["team_id"], "t1");
        assert_eq!(v["member_id"], "m1");
        assert_eq!(v["period"], "cumulative");
        assert_eq!(v["threshold_usd"], 1.0);
        assert!((v["usage_cost_usd"].as_f64().unwrap() - 1.1).abs() < 1e-9);

        // Already above the threshold: no re-alert (monotonic cost ⇒ once).
        assert!(!budget_alert_on_crossing(&mut conn, "t1", "m1", after, after + 0.1, 1.0).unwrap());
        assert_eq!(alert_count(&conn), 1);

        // Other members / teams are isolated from the sum.
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m2', 't1', 's2', 'm2', 'member', 'active', 'now')",
            [],
        )
        .unwrap();
        assert!((member_cost_sum(&conn, "t1", "m2").unwrap() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn budget_threshold_parsing() {
        assert_eq!(parse_budget_threshold("1.5"), Some(1.5));
        assert_eq!(parse_budget_threshold(" 2.0 "), Some(2.0));
        assert_eq!(parse_budget_threshold("0"), None);
        assert_eq!(parse_budget_threshold("-3"), None);
        assert_eq!(parse_budget_threshold("abc"), None);
        assert_eq!(parse_budget_threshold(""), None);
        assert_eq!(parse_budget_threshold("NaN"), None);
    }

    #[test]
    fn push_rejects_other_member_rows() {
        // This is enforced in serve dispatch, but ensure push itself doesn't
        // silently merge; it just inserts whatever it's given. The guard lives
        // in the RPC layer.
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let r = base_row("t1", "m2", "tool_call");
        push_activities(&mut conn, &[r]).unwrap();
        let s = summary(&conn, Some("t1"), None, None, None, None, None).unwrap();
        assert_eq!(s["overall"]["count"], 1);
    }

    #[test]
    fn tools_and_files_distributions() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let mut t1 = base_row("t1", "m1", "tool_call");
        t1.detail = Some(serde_json::json!({"tool":"bash","state":"completed"}));
        let mut t2 = base_row("t1", "m1", "tool_call");
        t2.detail = Some(serde_json::json!({"tool":"bash","state":"completed"}));
        let mut t3 = base_row("t1", "m1", "tool_call");
        t3.detail = Some(serde_json::json!({"tool":"edit","state":"completed"}));
        let mut f1 = base_row("t1", "m1", "file_edit");
        f1.detail = Some(serde_json::json!({"file":"src/main.rs"}));
        let mut f2 = base_row("t1", "m1", "file_edit");
        f2.detail = Some(serde_json::json!({"file":"src/main.rs"}));
        push_activities(&mut conn, &[t1, t2, t3, f1, f2]).unwrap();

        let tools = tools(&conn, Some("t1"), None, None, None, None).unwrap();
        assert_eq!(tools[0]["tool"], "bash");
        assert_eq!(tools[0]["count"], 2);
        assert_eq!(tools[1]["tool"], "edit");
        assert_eq!(tools[1]["count"], 1);

        let files = files(&conn, Some("t1"), None, None, None, None).unwrap();
        assert_eq!(files[0]["file"], "src/main.rs");
        assert_eq!(files[0]["count"], 2);
    }

    #[test]
    fn rows_newest_first_and_human_rows() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let mut a = base_row("t1", "m1", "tool_call");
        a.detail = Some(serde_json::json!({"tool":"bash"}));
        a.started_at = "2026-08-19T10:00:00Z".to_string();
        let mut b = base_row("t1", "m1", "human_command");
        b.detail = Some(serde_json::json!({"name":"/team","args":"status"}));
        b.started_at = "2026-08-19T11:00:00Z".to_string();
        push_activities(&mut conn, &[a, b]).unwrap();

        let rows = rows(&conn, Some("t1"), None, None, None, None, None, 10).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 2);
        assert_eq!(rows[0]["kind"], "human_command"); // newest first

        let h = human_rows(&conn, Some("t1"), None, None, None, None, 10).unwrap();
        assert_eq!(h.as_array().unwrap().len(), 1);
        assert_eq!(h[0]["kind"], "human_command");
    }

    #[test]
    fn compute_cost_basic_and_cache() {
        // 1M input @ $3, 500k output @ $15, 100k cache-read @ $0.30,
        // 50k cache-write @ $3.75. Non-cached input = 1M - 100k = 900k.
        let p = ModelPrice {
            provider_id: "anthropic".into(),
            model_id: "claude-sonnet-4-5".into(),
            price_input: 3.0,
            price_output: 15.0,
            price_cache_read: 0.30,
            price_cache_write: 3.75,
            over200k_input: None,
            over200k_output: None,
            over200k_cache_read: None,
            over200k_cache_write: None,
        };
        let b = p
            .compute(Some(1_000_000), Some(500_000), None, Some(100_000), Some(50_000))
            .unwrap();
        assert!((b.cost_input - 2.7).abs() < 1e-9, "cost_input={}", b.cost_input); // 900k * 3 / 1e6
        assert!((b.cost_output - 7.5).abs() < 1e-9, "cost_output={}", b.cost_output); // 500k * 15 / 1e6
        assert!((b.cost_cache_read - 0.03).abs() < 1e-9, "cache_read={}", b.cost_cache_read);
        assert!((b.cost_cache_write - 0.1875).abs() < 1e-9, "cache_write={}", b.cost_cache_write);
        assert!((b.total - (2.7 + 7.5 + 0.03 + 0.1875)).abs() < 1e-9);
    }

    #[test]
    fn compute_cost_no_tokens_returns_none() {
        let p = ModelPrice {
            provider_id: "x".into(),
            model_id: "y".into(),
            price_input: 1.0,
            price_output: 2.0,
            price_cache_read: 0.0,
            price_cache_write: 0.0,
            over200k_input: None,
            over200k_output: None,
            over200k_cache_read: None,
            over200k_cache_write: None,
        };
        assert!(p.compute(None, None, None, None, None).is_none());
    }

    #[test]
    fn compute_cost_over200k_tier() {
        // 300k input crosses the >200k tier; use the over200k input price.
        let p = ModelPrice {
            provider_id: "anthropic".into(),
            model_id: "c".into(),
            price_input: 3.0,
            price_output: 15.0,
            price_cache_read: 0.30,
            price_cache_write: 3.75,
            over200k_input: Some(6.0),
            over200k_output: Some(30.0),
            over200k_cache_read: Some(0.60),
            over200k_cache_write: Some(7.50),
        };
        let b = p.compute(Some(300_000), Some(10_000), None, Some(0), Some(0)).unwrap();
        // 300k * 6 / 1e6 = 1.8
        assert!((b.cost_input - 1.8).abs() < 1e-9, "cost_input={}", b.cost_input);
        assert!((b.cost_output - 0.3).abs() < 1e-9, "cost_output={}", b.cost_output);
    }

    #[test]
    fn apply_pricing_computes_and_marks_sources() {
        let conn = test_conn();
        seed_team(&conn, "t1", "m1");
        // A step_finish row without opencode cost, attributed to a seeded model.
        let mut step = base_row("t1", "m1", "step_finish");
        step.provider_id = Some("anthropic".to_string());
        step.model_id = Some("claude-sonnet-4-5".to_string());
        step.tokens_input = Some(1_000_000);
        step.tokens_output = Some(500_000);
        step.tokens_cache_read = Some(100_000);
        step.tokens_cache_write = Some(50_000);
        // A step_finish row with an opencode cost (must stay authoritative).
        let mut with_cost = base_row("t1", "m1", "step_finish");
        with_cost.provider_id = Some("anthropic".to_string());
        with_cost.model_id = Some("claude-sonnet-4-5".to_string());
        with_cost.tokens_input = Some(1_000_000);
        with_cost.tokens_output = Some(500_000);
        with_cost.cost = Some(12.34);
        // A non-step row (tool_call) must be marked 'none'.
        let tool = base_row("t1", "m1", "tool_call");

        let mut rows = vec![step, with_cost, tool];
        apply_pricing(&conn, &mut rows).unwrap();

        assert_eq!(rows[0].cost_source.as_deref(), Some("computed"));
        assert!(rows[0].cost.unwrap() > 0.0);
        assert!(rows[0].cost_input.unwrap() > 0.0);

        assert_eq!(rows[1].cost_source.as_deref(), Some("opencode"));
        assert_eq!(rows[1].cost.unwrap(), 12.34);
        // decomposition still filled for display
        assert!(rows[1].cost_input.is_some());

        assert_eq!(rows[2].cost_source.as_deref(), Some("none"));
        assert!(rows[2].cost.is_none());
    }

    #[test]
    fn apply_pricing_unknown_model_marks_none() {
        let conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let mut step = base_row("t1", "m1", "step_finish");
        step.provider_id = Some("nope".to_string());
        step.model_id = Some("unknown-model".to_string());
        step.tokens_input = Some(1000);
        let mut rows = vec![step];
        apply_pricing(&conn, &mut rows).unwrap();
        assert_eq!(rows[0].cost_source.as_deref(), Some("none"));
        assert!(rows[0].cost.is_none());
    }

    #[test]
    fn upsert_prices_owner_beats_opencode() {
        let conn = test_conn();
        // opencode sync first
        let oc = ModelPrice {
            provider_id: "openai".into(),
            model_id: "gpt-4o".into(),
            price_input: 2.5,
            price_output: 10.0,
            price_cache_read: 1.25,
            price_cache_write: 2.5,
            over200k_input: None,
            over200k_output: None,
            over200k_cache_read: None,
            over200k_cache_write: None,
        };
        upsert_prices(&conn, &[oc], "opencode").unwrap();
        // owner override
        let ow = ModelPrice {
            provider_id: "openai".into(),
            model_id: "gpt-4o".into(),
            price_input: 9.9,
            price_output: 9.9,
            price_cache_read: 0.0,
            price_cache_write: 0.0,
            over200k_input: None,
            over200k_output: None,
            over200k_cache_read: None,
            over200k_cache_write: None,
        };
        upsert_prices(&conn, &[ow], "owner").unwrap();
        let prices = load_prices(&conn).unwrap();
        let p = prices.get(&("openai".to_string(), "gpt-4o".to_string())).unwrap();
        assert_eq!(p.price_input, 9.9); // owner won
    }

    #[test]
    fn upsert_prices_opencode_does_not_overwrite_owner() {
        let conn = test_conn();
        let ow = ModelPrice {
            provider_id: "openai".into(),
            model_id: "gpt-4o".into(),
            price_input: 9.9,
            price_output: 9.9,
            price_cache_read: 0.0,
            price_cache_write: 0.0,
            over200k_input: None,
            over200k_output: None,
            over200k_cache_read: None,
            over200k_cache_write: None,
        };
        upsert_prices(&conn, &[ow], "owner").unwrap();
        let oc = ModelPrice {
            provider_id: "openai".into(),
            model_id: "gpt-4o".into(),
            price_input: 2.5,
            price_output: 10.0,
            price_cache_read: 1.25,
            price_cache_write: 2.5,
            over200k_input: None,
            over200k_output: None,
            over200k_cache_read: None,
            over200k_cache_write: None,
        };
        upsert_prices(&conn, &[oc], "opencode").unwrap();
        let prices = load_prices(&conn).unwrap();
        let p = prices.get(&("openai".to_string(), "gpt-4o".to_string())).unwrap();
        assert_eq!(p.price_input, 9.9); // opencode did NOT overwrite owner
    }

    #[test]
    fn presence_upsert_and_ttl() {
        let conn = test_conn();
        let p = Presence {
            active_sessions: 2,
            running_tools: 1,
            open_terminals: 1,
        };
        upsert_presence(&conn, "m1", &p).unwrap();
        let map = presence_for_members(&conn).unwrap();
        assert_eq!(map.get("m1").unwrap().active_sessions, 2);
        assert_eq!(map.get("m1").unwrap().running_tools, 1);
        assert_eq!(map.get("m1").unwrap().open_terminals, 1);

        // Simulate a stale row (updated_at far in the past) → dropped by TTL.
        conn.execute(
            "UPDATE presence SET updated_at = '2000-01-01T00:00:00+00:00' WHERE member_id = 'm1'",
            [],
        )
        .unwrap();
        let map = presence_for_members(&conn).unwrap();
        assert!(!map.contains_key("m1"), "stale presence should be dropped");
    }

    #[test]
    fn recent_activity_window() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let recent = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        let old = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let mut t1 = base_row("t1", "m1", "tool_call");
        t1.detail = Some(json!({"tool":"bash"}));
        t1.started_at = recent.clone();
        let mut w = base_row("t1", "m1", "work_session");
        w.started_at = recent.clone();
        let mut old_row = base_row("t1", "m1", "tool_call");
        old_row.detail = Some(json!({"tool":"edit"}));
        old_row.started_at = old;
        push_activities(&mut conn, &[t1, w, old_row]).unwrap();

        let r = recent_activity(&conn, Some("t1"), RECENT_WINDOW_MS).unwrap();
        assert_eq!(r["active_members"], 1);
        assert_eq!(r["tool_calls"], 1); // only the recent tool_call
        assert_eq!(r["work_sessions"], 1);
        assert_eq!(r["total_rows"], 2); // recent t1 + w (old row excluded)
    }
}
