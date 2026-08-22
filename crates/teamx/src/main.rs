mod broadcast;
mod cli;
mod commands;
mod db;
mod events;
mod loopx;
mod pki;
mod routes;
mod serve;
mod socks5;
mod state;
mod teamfile;
mod tunnel;
mod tunnel_client;
mod tun_cli;
mod tun_dev;
mod tun_dns;
mod tun_socks;
mod tun_stack;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    let cli = Cli::parse();
    // `teamx serve` runs forever and manages its own DB lifecycle; it bypasses
    // the normal open-once-per-invocation flow.
    if let Command::Serve(sc) = &cli.command {
        match serve::serve(sc) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("teamx error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let db_path = cli.db.clone().unwrap_or_else(db::default_db_path);

    let result = run(&cli, &db_path);
    match result {
        Ok(out) => {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            } else {
                print_human(&out);
            }
        }
        Err(e) => {
            eprintln!("teamx error: {e}");
            std::process::exit(1);
        }
    }
}

fn run(cli: &Cli, db_path: &std::path::Path) -> Result<serde_json::Value, String> {
    let mut conn = db::open(db_path).map_err(|e| format!("cannot open database {db_path:?}: {e}"))?;
    db::migrate(&conn).map_err(|e| format!("schema init failed: {e}"))?;
    commands::execute(cli, &mut conn).map_err(|e| e.to_string())
}

fn print_human(v: &serde_json::Value) {
    if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
        // simple action result: print key fields except ok
        let mut fields: Vec<String> = Vec::new();
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if k == "ok" || k == "note" {
                    continue;
                }
                if val.is_string() {
                    fields.push(format!("{k}: {}", val.as_str().unwrap()));
                } else if val.is_number() {
                    fields.push(format!("{k}: {val}"));
                }
            }
            if let Some(note) = obj.get("note").and_then(|n| n.as_str()) {
                fields.push(format!("note: {note}"));
            }
        }
        println!("{}", fields.join("\n"));
        return;
    }

    if let Some(teams) = v.get("teams").and_then(|t| t.as_array()) {
        if v.get("new_events").is_some() {
            println!("[sync]");
        } else if v.get("team_id").is_none() {
            println!("[teams]");
        }
        for (i, t) in teams.iter().enumerate() {
            if v.get("team_id").is_some() || v.get("new_events").is_some() {
                println!("-- team {} --", i + 1);
            }
            print_team_block(t);
        }
        if let Some(events) = v.get("new_events").and_then(|e| e.as_array()) {
            if !events.is_empty() {
                println!("[new events]");
                for e in events {
                    print_event_line(e);
                }
            }
        }
        return;
    }

    if let Some(events) = v.get("events").and_then(|e| e.as_array()) {
        if let Some(team_name) = v.get("team").and_then(|t| t.get("name")).and_then(|n| n.as_str()) {
            println!("[teamx log] {team_name}");
            for e in events {
                print_log_line(e);
            }
            return;
        }
        for e in events {
            print_event_line(e);
        }
        return;
    }

    if let Some(roles) = v.get("roles").and_then(|r| r.as_array()) {
        println!("[roles]");
        for r in roles {
            let key = r.get("key").and_then(|k| k.as_str()).unwrap_or("-");
            let label = r.get("label").and_then(|k| k.as_str()).unwrap_or("");
            let desc = r.get("description").and_then(|k| k.as_str()).unwrap_or("");
            println!("  {key} - {label}: {desc}");
        }
        return;
    }

    if v.get("loopx").is_some() {
        let lx = &v["loopx"];
        println!(
            "loopx project: {} (available: {})",
            lx.get("project").and_then(|x| x.as_str()).unwrap_or("-"),
            lx.get("available").and_then(|x| x.as_bool()).map(|b| if b { "yes" } else { "no" }).unwrap_or("no")
        );
        for key in ["goal_state", "gate", "next_todo", "quota"] {
            if let Some(s) = lx.get(key).and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    println!("  {key}: {s}");
                }
            }
        }
        if let Some(e) = lx.get("error").and_then(|x| x.as_str()) {
            println!("  error: {e}");
        }
        return;
    }

    println!("{}", serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into()));
}

fn print_team_block(t: &serde_json::Value) {
    let team = t.get("team").cloned().unwrap_or_else(|| serde_json::json!({}));
    println!(
        "team: {} ({}) [{}]",
        team.get("name").and_then(|n| n.as_str()).unwrap_or("-"),
        team.get("id").and_then(|n| n.as_str()).unwrap_or("-"),
        team.get("state").and_then(|n| n.as_str()).unwrap_or("-"),
    );
    if let Some(goal) = t.get("goal") {
        println!(
            "goal: {} [{}]",
            goal.get("title").and_then(|n| n.as_str()).unwrap_or("-"),
            goal.get("state").and_then(|n| n.as_str()).unwrap_or("-"),
        );
    }
    if let Some(members) = t.get("members").and_then(|m| m.as_array()) {
        println!("members:");
        for m in members {
            println!(
                "  - {} (role: {}, state: {}){}",
                m.get("display_name").and_then(|n| n.as_str()).unwrap_or("-"),
                m.get("role").and_then(|n| n.as_str()).unwrap_or("-"),
                m.get("state").and_then(|n| n.as_str()).unwrap_or("-"),
                m.get("loopx_project")
                    .and_then(|n| n.as_str())
                    .map(|p| format!(" [loopx: {p}]"))
                    .unwrap_or_default(),
            );
        }
    }
    if let Some(questions) = t.get("questions").and_then(|q| q.as_array()) {
        let open: Vec<&serde_json::Value> = questions
            .iter()
            .filter(|q| q.get("state").and_then(|s| s.as_str()) == Some("open"))
            .collect();
        if !open.is_empty() {
            println!("open questions:");
            for q in open {
                println!(
                    "  - [{}] {} -> {}: {}",
                    q.get("id").and_then(|n| n.as_str()).unwrap_or("-"),
                    q.get("asker_member_id").and_then(|n| n.as_str()).unwrap_or("-"),
                    q.get("target_member_id").and_then(|n| n.as_str()).unwrap_or("-"),
                    q.get("question").and_then(|n| n.as_str()).unwrap_or("-"),
                );
            }
        }
    }
    if let Some(events) = t.get("recent_events").and_then(|e| e.as_array()) {
        if !events.is_empty() {
            println!("recent events:");
            for e in events.iter().take(8) {
                print_event_line(e);
            }
        }
    }
}

fn print_event_line(e: &serde_json::Value) {
    let seq = e.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);
    let ty = e.get("type").and_then(|t| t.as_str()).unwrap_or("-");
    let member = e.get("member_id").and_then(|m| m.as_str()).unwrap_or("-");
    let payload = e.get("payload").cloned().unwrap_or(serde_json::json!(null));
    let payload_str = if payload.is_null() {
        String::new()
    } else {
        format!(" {}", serde_json::to_string(&payload).unwrap_or_default())
    };
    println!("  #{seq:>4} {ty:<28} by {member}{payload_str}");
}

/// Human line for `teamx log` output: member resolved to a display name.
fn print_log_line(e: &serde_json::Value) {
    let seq = e.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);
    let ty = e.get("type").and_then(|t| t.as_str()).unwrap_or("-");
    let member = e.get("member").and_then(|m| m.as_str()).unwrap_or("-");
    let payload = e.get("payload").cloned().unwrap_or(serde_json::json!(null));
    let payload_str = if payload.is_null() {
        String::new()
    } else {
        format!(" {}", serde_json::to_string(&payload).unwrap_or_default())
    };
    println!("  #{seq:>4} {ty:<28} {member}{payload_str}");
}
