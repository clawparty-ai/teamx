//! credit 评分模型（design/018 · ADR-0004）。
//!
//! credit = **团队级信用**，由既有台账事件 + TCoin 余额**派生**（系统唯一写者，无人可手改）。
//!
//! 结构（与 TCoin 两级账本对称）：
//! - **租户侧**（`tenant_signals`）：履约行为的原始计数（events / members / goals）—— 不含评分；
//! - **平台侧**（`platform_signals`）：治理与财务信号（加入申请决定、TCoin 余额）；
//! - **合并打分**（`score`，纯函数）：事件加权累计 → 0–100 分 + S/A/B/C/D 分档 + 构成明细；
//! - **物化**（`refresh`）：把团队级快照写进平台库 `team_credit`，供**目录只读平台库**（ADR-0001）。
//!
//! 口径（SD-6…SD-15，**可调参数集中在 `W`**，调整须记版本）：
//! 滚动 90 天线性衰减、基线 60、事件去重、单日正向封顶、<5 事件显示"数据不足"。

use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// 模型版本（权重/阈值调整时必须递增，历史分数才可解释）。
pub const MODEL_VERSION: &str = "credit-v1";

/// 滚动窗口（天）—— 只有窗口内的事件参与评分。
pub const WINDOW_DAYS: i64 = 90;
/// 有效事件数低于此值时显示"数据不足"，不显示分数。
pub const MIN_EVENTS: i64 = 5;
/// 基线分（新团队 / 无信号时的起点）。
pub const BASELINE: i64 = 60;

/// 权重表（**可调参数**，改动须升 `MODEL_VERSION`）。
pub struct Weights {
    /// 每个**去重**的 `doc.verified` 完成：+分
    pub verified_each: f64,
    /// `doc.done → doc.verified` 转化率：满分（×rate）
    pub conversion_max: f64,
    /// 批准后**实际入场率**：满分（×rate）
    pub onboarding_max: f64,
    /// 成员留存：满分（×min(1, avg_tenure/30天)）
    pub retention_max: f64,
    /// 无欠费：+分
    pub no_arrears: f64,
    /// 欠费（TCoin 余额 < 0）：-分
    pub arrears_penalty: f64,
    /// 每个被拒的加入申请：-分（**分类封顶**）
    pub rejected_request_each: f64,
    /// 「申请被拒」类封顶
    pub rejected_request_cap: f64,
    /// 每个 `doc.rejected`：-分（分类封顶）
    pub doc_rejected_each: f64,
    /// 「任务被驳回」类封顶
    pub doc_rejected_cap: f64,
    /// 每个长期未处理的 blocked 目标：-分（分类封顶）
    pub stale_blocked_each: f64,
    /// 「长期阻塞」类封顶
    pub stale_blocked_cap: f64,
    /// **单日正向封顶**（防刷，SD-14）
    pub daily_positive_cap: f64,
}

pub const W: Weights = Weights {
    verified_each: 3.0,
    conversion_max: 10.0,
    onboarding_max: 8.0,
    retention_max: 6.0,
    no_arrears: 4.0,
    arrears_penalty: 10.0,
    rejected_request_each: 4.0,
    rejected_request_cap: 12.0,
    doc_rejected_each: 3.0,
    doc_rejected_cap: 9.0,
    stale_blocked_each: 6.0,
    stale_blocked_cap: 12.0,
    daily_positive_cap: 10.0,
};

/// blocked 目标视为"长期未处理"的天数阈值。
pub const STALE_BLOCKED_DAYS: i64 = 14;

/// 两侧信号的合并输入。
#[derive(Debug, Clone, Default)]
pub struct Signals {
    // ---- 租户侧（行为）----
    /// 去重后的 `doc.verified` 文档数（已按窗口+衰减加权）
    pub verified_weighted: f64,
    /// 去重后的 `doc.verified` 文档数（原始条数，用于"数据不足"判定与展示）
    pub verified_count: i64,
    /// 窗口内出现过的 `doc.done` 去重文档数
    pub done_count: i64,
    /// `doc.verified` 去重文档数（与 verified_count 同源的比值分子）
    pub verified_doc_count: i64,
    /// `doc.rejected` 条数（窗口内，已衰减）
    pub doc_rejected_weighted: f64,
    pub doc_rejected_count: i64,
    /// 长期未处理的 blocked 目标数
    pub stale_blocked_count: i64,
    /// 在册成员数
    pub members_active: i64,
    /// 在册成员平均在册天数
    pub avg_tenure_days: f64,
    // ---- 平台侧（治理/财务）----
    /// 已批准的加入申请数
    pub approved_requests: i64,
    /// 已入场的成员数（租户侧在册成员中来自申请批准的）
    pub onboarded: i64,
    /// 被拒的加入申请数
    pub rejected_requests: i64,
    /// TCoin 余额（负 = 欠费）
    pub tcoin_balance: i64,
    /// 有效事件总数（用于"数据不足"判定；不含负向？—— 含全部窗口内事件）
    pub event_count: i64,
}

/// 构成明细的一行。
fn row(signal: &str, count: String, points: f64, note: &str) -> Value {
    json!({ "signal": signal, "count": count, "points": (points * 10.0).round() / 10.0, "note": note })
}

/// 分档（SD-9）：S≥90 / A≥80 / B≥60 / C≥40 / D<40。
pub fn grade_of(score: i64) -> &'static str {
    match score {
        s if s >= 90 => "S",
        s if s >= 80 => "A",
        s if s >= 60 => "B",
        s if s >= 40 => "C",
        _ => "D",
    }
}

/// 打分（**纯函数**，可重复计算、无副作用）。
///
/// 返回 `(score, grade, insufficient, breakdown)`。
pub fn score(s: &Signals) -> (i64, &'static str, bool, Vec<Value>) {
    let mut bd: Vec<Value> = Vec::new();
    let mut pts = BASELINE as f64;

    // ---- 正向 ----
    let verified_pts = s.verified_weighted * W.verified_each;
    let daily_cap = W.daily_positive_cap;
    let _ = daily_cap; // 单日封顶在采集侧按日截断（见 tenant_signals）
    pts += verified_pts;
    if s.verified_count > 0 {
        bd.push(row(
            "完成（doc.verified）",
            format!("{} 个", s.verified_count),
            verified_pts,
            "90 天窗口内去重 + 线性衰减",
        ));
    }
    if s.done_count > 0 {
        let rate = (s.verified_doc_count as f64 / s.done_count as f64).min(1.0);
        let p = rate * W.conversion_max;
        pts += p;
        bd.push(row(
            "完成转化率",
            format!("{}/{} = {:.0}%", s.verified_doc_count, s.done_count, rate * 100.0),
            p,
            "done→verified 转化",
        ));
    }
    if s.approved_requests > 0 {
        let rate = (s.onboarded as f64 / s.approved_requests as f64).min(1.0);
        let p = rate * W.onboarding_max;
        pts += p;
        bd.push(row(
            "批准后入场率",
            format!("{}/{} = {:.0}%", s.onboarded, s.approved_requests, rate * 100.0),
            p,
            "批准即拉 Pod 的实际落地",
        ));
    }
    if s.members_active > 0 {
        let ratio = (s.avg_tenure_days / 30.0).min(1.0);
        let p = ratio * W.retention_max;
        pts += p;
        bd.push(row(
            "成员留存",
            format!("{} 人在册 · 平均 {:.0} 天", s.members_active, s.avg_tenure_days),
            p,
            "在册成员与平均在册时长",
        ));
    }
    if s.tcoin_balance >= 0 {
        pts += W.no_arrears;
        bd.push(row("无欠费", format!("余额 {}", s.tcoin_balance), W.no_arrears, "TCoin 非负"));
    }

    // ---- 负向（有封顶）----
    let mut neg = 0.0;
    if s.tcoin_balance < 0 {
        neg += W.arrears_penalty;
        bd.push(row(
            "欠费",
            format!("余额 {}", s.tcoin_balance),
            -W.arrears_penalty,
            "TCoin 余额为负",
        ));
    }
    if s.rejected_requests > 0 {
        let p = (s.rejected_requests as f64 * W.rejected_request_each).min(W.rejected_request_cap);
        neg += p;
        bd.push(row(
            "申请被拒",
            format!("{} 次", s.rejected_requests),
            -p,
            "owner 拒绝的加入申请（冷却期内）",
        ));
    }
    if s.doc_rejected_count > 0 {
        let p = (s.doc_rejected_weighted * W.doc_rejected_each).min(W.doc_rejected_cap);
        neg += p;
        bd.push(row(
            "任务被驳回",
            format!("{} 次", s.doc_rejected_count),
            -p,
            "doc.rejected（90 天窗口）",
        ));
    }
    if s.stale_blocked_count > 0 {
        let p = (s.stale_blocked_count as f64 * W.stale_blocked_each).min(W.stale_blocked_cap);
        neg += p;
        bd.push(row(
            "长期阻塞",
            format!("{} 个目标", s.stale_blocked_count),
            -p,
            &format!("blocked 超过 {STALE_BLOCKED_DAYS} 天未处理"),
        ));
    }
    // 负向按**分类封顶**累加（最大 10 + 12 + 9 + 12 = 43 → 极端可到 17 分 = D 档，
    // 而不是把所有人都压在 C 档以上）。
    pts -= neg.min(
        W.arrears_penalty + W.rejected_request_cap + W.doc_rejected_cap + W.stale_blocked_cap,
    );

    let score = pts.round().clamp(0.0, 100.0) as i64;
    let insufficient = s.event_count < MIN_EVENTS;
    (score, grade_of(score), insufficient, bd)
}

// ---------------------------------------------------------------------------
// 采集
// ---------------------------------------------------------------------------

fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(s).ok()
}

/// 线性衰减权重：窗口内 1.0 → 窗口边界 0.0。
fn decay(created_at: &str, now: chrono::DateTime<chrono::FixedOffset>) -> f64 {
    match parse_ts(created_at) {
        Some(t) => {
            let age_days = (now - t).num_seconds() as f64 / 86400.0;
            (1.0 - age_days / WINDOW_DAYS as f64).clamp(0.0, 1.0)
        }
        None => 0.0,
    }
}

/// 租户侧信号（只读租户 DB；**不含评分**）。
pub fn tenant_signals(conn: &Connection, team_id: &str) -> Result<Signals, String> {
    let now = chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(0).unwrap());
    let since = (now - chrono::Duration::days(WINDOW_DAYS)).to_rfc3339();
    let mut s = Signals::default();

    // 事件：窗口内 + 去重（按 payload 的 doc id）+ 单日正向封顶（防刷，SD-14）
    let mut stmt = conn
        .prepare(
            "SELECT type, COALESCE(payload_json,''), COALESCE(member_id,''), created_at
             FROM events WHERE team_id = ?1 AND created_at >= ?2",
        )
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![team_id, since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| format!("db: {e}"))?;

    let mut verified_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut done_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut verified_weighted = 0.0f64;
    let mut doc_rejected_weighted = 0.0f64;
    let mut doc_rejected_count = 0i64;
    let mut event_count = 0i64;
    // 单日正向封顶：按 (日期, member) 累加正向贡献，超出部分不计数
    let mut daily: std::collections::HashMap<String, f64> = std::collections::HashMap::new();

    for row in rows {
        let (ty, payload, member_id, created_at) = row.map_err(|e| format!("db: {e}"))?;
        event_count += 1;
        // doc id：payload 里的 `id`；缺失则退化为 payload 原文（仍然可比）
        let doc_id = serde_json::from_str::<Value>(&payload)
            .ok()
            .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(str::to_string))
            .unwrap_or_else(|| payload.clone());
        let w = decay(&created_at, now);
        match ty.as_str() {
            "doc.verified" => {
                if verified_ids.insert(doc_id.clone()) {
                    // 单日（member+日期）正向封顶
                    let day = created_at.get(0..10).unwrap_or("").to_string();
                    let key = format!("{member_id}|{day}");
                    let used = daily.entry(key).or_insert(0.0);
                    let add = (w * W.verified_each).min((W.daily_positive_cap - *used).max(0.0));
                    *used += add;
                    verified_weighted += add / W.verified_each; // 归一回"权重化个数"
                }
            }
            "doc.done" => {
                done_ids.insert(doc_id);
            }
            "doc.rejected" => {
                doc_rejected_count += 1;
                doc_rejected_weighted += w;
            }
            _ => {}
        }
    }
    s.verified_count = verified_ids.len() as i64;
    s.verified_doc_count = verified_ids.len() as i64;
    s.verified_weighted = verified_weighted;
    s.done_count = done_ids.len() as i64;
    s.doc_rejected_count = doc_rejected_count;
    s.doc_rejected_weighted = doc_rejected_weighted;
    s.event_count = event_count;

    // 成员留存（在册成员 + 平均在册天数）
    let mut mstmt = conn
        .prepare(
            "SELECT joined_at, left_at FROM members
             WHERE team_id = ?1 AND state NOT IN ('left','denied','pending')",
        )
        .map_err(|e| format!("db: {e}"))?;
    let mrows = mstmt
        .query_map(params![team_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .map_err(|e| format!("db: {e}"))?;
    let mut total_days = 0.0f64;
    let mut n = 0i64;
    for m in mrows {
        let (joined, left) = m.map_err(|e| format!("db: {e}"))?;
        if let Some(j) = parse_ts(&joined) {
            let end = left.as_deref().and_then(parse_ts).unwrap_or(now);
            total_days += ((end - j).num_seconds() as f64 / 86400.0).max(0.0);
            n += 1;
        }
    }
    s.members_active = n;
    s.avg_tenure_days = if n > 0 { total_days / n as f64 } else { 0.0 };

    // 长期未处理的 blocked 目标
    s.stale_blocked_count = conn
        .query_row(
            "SELECT COUNT(*) FROM goals
             WHERE team_id = ?1 AND state = 'blocked'
               AND updated_at < ?2",
            params![
                team_id,
                (now - chrono::Duration::days(STALE_BLOCKED_DAYS)).to_rfc3339()
            ],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(s)
}

/// 平台侧信号（治理与财务）：加入申请决定 + TCoin 余额。
pub fn platform_signals(conn: &Connection, team_id: &str) -> Result<Signals, String> {
    let count = |status: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM team_join_requests WHERE team_id = ?1 AND status = ?2",
            params![team_id, status],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    Ok(Signals {
        approved_requests: count("approved"),
        rejected_requests: count("rejected"),
        tcoin_balance: crate::tcoin::balance(conn, team_id, "tenant").unwrap_or(0),
        ..Default::default()
    })
}

/// 合并两侧信号（平台侧覆盖/补齐字段，租户侧行为数据保留）。
pub fn merge(tenant: Signals, platform: Signals) -> Signals {
    Signals {
        approved_requests: platform.approved_requests,
        // 批准后"实际入场"：租户侧在册成员数不超过已批准数时，取在册数；
        // 没有申请记录（owner 直接邀请）时不算入分母（rate 不评）。
        onboarded: if platform.approved_requests > 0 {
            tenant.members_active.min(platform.approved_requests)
        } else {
            0
        },
        rejected_requests: platform.rejected_requests,
        tcoin_balance: platform.tcoin_balance,
        ..tenant
    }
}

// ---------------------------------------------------------------------------
// 物化（平台库快照）—— 目录只读平台库（ADR-0001）
// ---------------------------------------------------------------------------

/// 写入/刷新团队级快照（**平台库**）。
pub fn materialize(conn: &Connection, team_id: &str, sc: &Signals) -> Result<Value, String> {
    let (score, grade, insufficient, breakdown) = score(sc);
    let at = crate::db::now();
    conn.execute(
        "INSERT INTO team_credit (team_id, score, grade, insufficient, event_count,
                                  breakdown_json, model_version, computed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(team_id) DO UPDATE SET
           score=excluded.score, grade=excluded.grade, insufficient=excluded.insufficient,
           event_count=excluded.event_count, breakdown_json=excluded.breakdown_json,
           model_version=excluded.model_version, computed_at=excluded.computed_at",
        params![
            team_id,
            score,
            grade,
            if insufficient { 1 } else { 0 },
            sc.event_count,
            serde_json::to_string(&breakdown).unwrap_or_else(|_| "[]".to_string()),
            MODEL_VERSION,
            at
        ],
    )
    .map_err(|e| format!("db: {e}"))?;
    Ok(json!({
        "team_id": team_id, "score": score, "grade": grade,
        "insufficient": insufficient, "event_count": sc.event_count,
        "breakdown": breakdown, "model_version": MODEL_VERSION, "computed_at": at,
    }))
}

/// 读团队级快照（平台库）。无快照 → None。
pub fn snapshot(conn: &Connection, team_id: &str) -> Result<Option<Value>, String> {
    let row: Option<(i64, String, i64, i64, String, String, String)> = conn
        .query_row(
            "SELECT score, grade, insufficient, event_count, breakdown_json,
                    model_version, computed_at
             FROM team_credit WHERE team_id = ?1",
            params![team_id],
            |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?,
                ))
            },
        )
        .ok();
    Ok(row.map(|(score, grade, insuff, events, bd, mv, at)| {
        json!({
            "team_id": team_id, "score": score, "grade": grade,
            "insufficient": insuff != 0, "event_count": events,
            "breakdown": serde_json::from_str::<Value>(&bd).unwrap_or(json!([])),
            "model_version": mv, "computed_at": at,
        })
    }))
}

/// 只取**公开可见**的分档（SD-5：对外只给徽章；数据不足时不给分档）。
pub fn public_grade(conn: &Connection, team_id: &str) -> Value {
    match snapshot(conn, team_id) {
        Ok(Some(v)) if v["insufficient"] == json!(false) => json!(v["grade"]),
        _ => json!(null),
    }
}

/// 列出全部快照（目录/运维用）。
pub fn list(conn: &Connection) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT team_id, score, grade, insufficient, event_count, computed_at
             FROM team_credit ORDER BY score DESC",
        )
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({
                "team_id": r.get::<_, String>(0)?,
                "score": r.get::<_, i64>(1)?,
                "grade": r.get::<_, String>(2)?,
                "insufficient": r.get::<_, i64>(3)? != 0,
                "event_count": r.get::<_, i64>(4)?,
                "computed_at": r.get::<_, String>(5)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("db: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Signals {
        Signals {
            event_count: 10,
            members_active: 3,
            avg_tenure_days: 30.0,
            tcoin_balance: 100,
            ..Default::default()
        }
    }

    #[test]
    fn grade_thresholds() {
        assert_eq!(grade_of(100), "S");
        assert_eq!(grade_of(90), "S");
        assert_eq!(grade_of(89), "A");
        assert_eq!(grade_of(80), "A");
        assert_eq!(grade_of(79), "B");
        assert_eq!(grade_of(60), "B");
        assert_eq!(grade_of(59), "C");
        assert_eq!(grade_of(39), "D");
        assert_eq!(grade_of(0), "D");
    }

    #[test]
    fn baseline_when_no_signals_and_insufficient_flag() {
        let s = Signals { event_count: 0, ..Default::default() };
        let (score, grade, insufficient, bd) = score(&s);
        // 无欠费会给一个正向分（余额 0 视为非负）→ 基线 + no_arrears
        assert_eq!(score, BASELINE + 4);
        assert_eq!(grade, "B");
        assert!(insufficient, "0 事件必须标记数据不足");
        assert!(bd.iter().any(|r| r["signal"] == "无欠费"));
    }

    #[test]
    fn insufficient_threshold_is_five_events() {
        let mut s = base();
        s.event_count = MIN_EVENTS - 1;
        assert!(score(&s).2);
        s.event_count = MIN_EVENTS;
        assert!(!score(&s).2);
    }

    #[test]
    fn positives_raise_and_conversion_rate_is_ratio() {
        let s = Signals {
            event_count: 20,
            verified_count: 4,
            verified_doc_count: 4,
            verified_weighted: 4.0, // 全新鲜（衰减=1）
            done_count: 8,
            members_active: 2,
            avg_tenure_days: 60.0,
            approved_requests: 4,
            onboarded: 3,
            tcoin_balance: 500,
            ..Default::default()
        };
        let (score, grade, insuff, bd) = score(&s);
        assert!(!insuff);
        assert!(score > BASELINE, "正信号应高于基线: {score}");
        assert_eq!(grade_of(score), grade);
        // 构成含转化率（4/8 = 50% → 5 分）
        let conv = bd.iter().find(|r| r["signal"] == "完成转化率").unwrap();
        assert_eq!(conv["points"], json!(5.0));
        // 入场率 3/4 = 75% → 6 分
        let onb = bd.iter().find(|r| r["signal"] == "批准后入场率").unwrap();
        assert_eq!(onb["points"], json!(6.0));
    }

    #[test]
    fn negatives_are_capped() {
        let s = Signals {
            event_count: 10,
            tcoin_balance: -1,
            rejected_requests: 100,
            doc_rejected_count: 100,
            doc_rejected_weighted: 100.0,
            stale_blocked_count: 100,
            ..Default::default()
        };
        let (sc, grade, _, bd) = score(&s);
        // 欠费 10 + 拒绝封顶 12 + 驳回封顶 9 + 阻塞封顶 12 = -43 → 60-43 = 17
        assert_eq!(sc, 17, "负向须按分类封顶（不叠加为无界）");
        assert_eq!(grade, "D", "严重失信必须能落到 D 档");
        assert!(!bd.is_empty());
    }

    #[test]
    fn arrears_removes_the_no_arrears_bonus() {
        let mut with = base();
        with.tcoin_balance = 0;
        let mut without = base();
        without.tcoin_balance = -5;
        let (a, ..) = score(&with);
        let (b, ..) = score(&without);
        assert!(a > b);
        assert_eq!(a - b, 14, "无欠费 +4 与欠费 -10 的差");
    }

    fn mem_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        c.execute(
            "INSERT INTO teams (id,name,owner_member_id,goal_id,state,invite_token,created_at,updated_at)
             VALUES ('t1','T',NULL,NULL,'active','tok','2026-01-01T00:00:00+00:00','2026-01-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO members (id,team_id,session_key,display_name,state,joined_at)
             VALUES ('m1','t1','s1','甲','active','2026-08-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
        c
    }

    fn ev(c: &Connection, ty: &str, payload: &str, at: &str) {
        let seq: i64 = c
            .query_row(
                "SELECT COALESCE(MAX(seq),0)+1 FROM events WHERE team_id='t1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        c.execute(
            "INSERT INTO events (team_id, member_id, seq, type, payload_json, created_at)
             VALUES ('t1','m1',?1,?2,?3,?4)",
            params![seq, ty, payload, at],
        )
        .unwrap();
    }

    #[test]
    fn tenant_signals_dedupe_by_doc_id_and_count_all_events() {
        let c = mem_db();
        let fresh = chrono::Utc::now().to_rfc3339();
        // 3 个不同的 veverified + 1 个重复 → verified_count=3
        for id in ["a", "b", "c", "a"] {
            ev(&c, "doc.verified", &format!("{{\"doc\":\"taskx\",\"id\":\"{id}\"}}"), &fresh);
        }
        for id in ["a", "b", "c", "d", "e"] {
            ev(&c, "doc.done", &format!("{{\"doc\":\"taskx\",\"id\":\"{id}\"}}"), &fresh);
        }
        ev(&c, "doc.rejected", "{\"doc\":\"taskx\",\"id\":\"r1\"}", &fresh);
        // 一个 14 天前就 blocked 的目标
        c.execute(
            "INSERT INTO goals (id,team_id,title,state,updated_at,created_at)
             VALUES ('g1','t1','g','blocked','2026-01-01T00:00:00+00:00','2026-01-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
        let s = tenant_signals(&c, "t1").unwrap();
        assert_eq!(s.verified_count, 3, "按 doc id 去重");
        assert_eq!(s.done_count, 5);
        assert_eq!(s.doc_rejected_count, 1);
        assert_eq!(s.stale_blocked_count, 1);
        assert_eq!(s.members_active, 1);
        assert_eq!(s.event_count, 10, "事件总数含全部类型");
        assert!(s.verified_weighted > 2.9 && s.verified_weighted <= 3.0, "新鲜事件衰减≈1: {}", s.verified_weighted);
    }

    #[test]
    fn old_events_decay_and_fall_outside_window() {
        let c = mem_db();
        let old = (chrono::Utc::now() - chrono::Duration::days(89)).to_rfc3339();
        let outside = (chrono::Utc::now() - chrono::Duration::days(120)).to_rfc3339();
        ev(&c, "doc.verified", "{\"doc\":\"taskx\",\"id\":\"old\"}", &old);
        ev(&c, "doc.verified", "{\"doc\":\"taskx\",\"id\":\"out\"}", &outside);
        let s = tenant_signals(&c, "t1").unwrap();
        assert_eq!(s.verified_count, 1, "窗口外事件不参与");
        assert!(s.verified_weighted < 0.1, "89 天前应大幅衰减: {}", s.verified_weighted);
    }

    #[test]
    fn materialize_snapshot_and_public_grade() {
        let c = mem_db();
        let fresh = chrono::Utc::now().to_rfc3339();
        for id in ["a", "b", "c", "d", "e"] {
            ev(&c, "doc.verified", &format!("{{\"doc\":\"taskx\",\"id\":\"{id}\"}}"), &fresh);
        }
        let s = tenant_signals(&c, "t1").unwrap();
        let merged = merge(s, platform_signals(&c, "t1").unwrap());
        let out = materialize(&c, "t1", &merged).unwrap();
        assert!(out["score"].as_i64().unwrap() > BASELINE);
        assert_eq!(out["insufficient"], json!(false));
        let snap = snapshot(&c, "t1").unwrap().unwrap();
        assert_eq!(snap["model_version"], json!(MODEL_VERSION));
        assert!(snap["breakdown"].as_array().is_some_and(|a| !a.is_empty()));
        assert_eq!(public_grade(&c, "t1"), snap["grade"]);
        // 数据不足 → 公开分档为 null（不给新团队贴标签 SD-11）
        let c2 = mem_db();
        ev(&c2, "doc.created", "{\"doc\":\"taskx\",\"id\":\"x\"}", &fresh);
        let s2 = tenant_signals(&c2, "t1").unwrap();
        materialize(&c2, "t1", &merge(s2, platform_signals(&c2, "t1").unwrap())).unwrap();
        assert_eq!(public_grade(&c2, "t1"), json!(null));
    }

    #[test]
    fn platform_signals_read_join_requests_and_arrears() {
        let c = mem_db();
        for (id, st) in [("r1", "approved"), ("r2", "approved"), ("r3", "rejected")] {
            c.execute(
                "INSERT INTO team_join_requests (id,team_id,user_id,status,created_at)
                 VALUES (?1,'t1','u1',?2,'2026-01-01T00:00:00+00:00')",
                params![id, st],
            )
            .unwrap();
        }
        crate::tcoin::grant(&c, "t1", 100, "test", "p").unwrap();
        crate::tcoin::settle(&c, "t1", 250, "b1", "p").unwrap(); // 余额 -150（欠费）
        let ps = platform_signals(&c, "t1").unwrap();
        assert_eq!(ps.approved_requests, 2);
        assert_eq!(ps.rejected_requests, 1);
        assert_eq!(ps.tcoin_balance, -150);
        // 合并：入场率分母用已批准数
        let ts = Signals { members_active: 1, ..Default::default() };
        let m = merge(ts, ps);
        assert_eq!(m.onboarded, 1);
    }

    #[test]
    fn score_is_deterministic_and_clamped() {
        let mut s = base();
        s.verified_weighted = 1000.0;
        s.verified_count = 1000;
        let (x, ..) = score(&s);
        let (y, ..) = score(&s);
        assert_eq!(x, y);
        assert!((0..=100).contains(&x));
        // 负向下限：基线 60 − 分类封顶合计 43 = 17（模型 floor，D 档）
        let s2 = Signals {
            event_count: 10,
            tcoin_balance: -100000,
            rejected_requests: 1000,
            doc_rejected_count: 1000,
            doc_rejected_weighted: 1000.0,
            stale_blocked_count: 1000,
            ..Default::default()
        };
        assert_eq!(score(&s2).0, 17, "严重失信触底 17（D 档），且不得为负");
    }
}
