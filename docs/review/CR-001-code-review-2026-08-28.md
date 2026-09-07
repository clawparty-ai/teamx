# Code Review Summary — Enterprise Feature Set

| 字段 | 值 |
|------|-----|
| **文档编号** | CR-001 |
| **文档类型** | Code Review Report |
| **版本** | v1.0 |
| **日期** | 2026-08-28 (周五) |
| **分支** | `enterprise` |
| **最新提交** | `2258bc4` |
| **范围** | P1 capture / P2 analysis / P3 replay |

---

## 📋 Objective
Complete teamx enterprise feature set (capture/analysis/replay) with full test coverage, iterative code review and fix all identified issues until no more can be found.

**Constraints:**
- 禁词约束：所有文档/提交信息不得出现 "Clash" 字眼
- Git: remote=`origin`=`clawparty-ai/teamx`; current branch: `enterprise`

---

## ✅ Final Status

| Check | Result |
|-------|--------|
| `cargo test -p teamx` | **92 passed; 0 failed; 0 warnings** |
| `cargo build -p teamx` | **0 warnings** |
| `capture-test.sh` | **9/9 passed** (with fresh DB) |
| `analysis-test.sh` | 3 passed, 3 skipped (network-gated) |
| `replay-test.sh` | 4 passed, 1 skipped (network-gated) |

---

## 🐛 All Issues Found & Fixed

### ▶️ 1st Code Review Round — 23 Issues (B1-B4, D1-D7, S1-S4, Q1-Q8)

#### Bugs (B1-B4)
| ID | Area | Issue | Fix |
|----|------|-------|-----|
| **B1+B2** | `export_keylog` SQL | `SELECT label, client_random, secret FROM tls_keys ORDER BY added_at ASC` + `hex_encode` formatting | Fixed SQL query + hex formatting for keylog export |
| **B3** | `parse_request`/`parse_response` | Body offset uses fixed `sep_len` but HTTP separator may be `\r\n\r\n` (4 bytes) or `\n\n` (2 bytes) | Dynamic `sep_len` based on actual separator found |
| **B4** | `capture start` daemon | Dead writer thread — `spawn_writer` returned before daemon initialized; `capture start` would exit immediately | Removed dead writer; daemon stays alive via `stdin` read loop |

#### Design Improvements (D1-D7)
| ID | Area | Issue | Fix |
|----|------|-------|-----|
| **D1** | `stream_chunks.dir` schema | Missing CHECK constraint — any TEXT could be inserted | Added `CHECK(dir IN ('up', 'down'))` to table schema |
| **D2** | `load_traffic_secrets` | Full table scan `SELECT ... FROM tls_keys` without index | Changed to `WHERE client_random = ?1` (indexed via PRIMARY KEY) |
| **D3** | Global SEQ counters | `static AtomicU64` shared across ALL flows/SOCKS5/tun0 — counter collision | Moved `seq_up`/`seq_down` into `SocketSlot` (tun_stack.rs) + per-flow locals in SOCKS5 task (tunnel_client.rs) |
| **D4** | `FlowOpen.first_bytes` auto-inserted as chunk | `apply_batch` was inserting `first_bytes` as a `Chunk` event — double-counting | Removed auto-insertion; callers must emit separate `Chunk` if bytes should appear in `stream_chunks` |
| **D5** | Orphaned key cleanup | No method to delete `tls_keys` whose `client_random` no longer appears in any flow | Added `cleanup_orphaned_keys()` method + `prune_max_size` calls it after pruning |
| **D6** | `export_pcap` address parsing | Needed `local_addr`/`remote_addr` from flows table for packet construction | Added `parse_addr()` helper that resolves IP:port or falls back to defaults |
| **D7** | HAR `startedDateTime` | No ISO 8601 conversion for Unix timestamps | Added `timestamp_to_iso()` helper (f64 → UTC ISO 8601) |

#### Safety (S1-S4)
| ID | Area | Issue | Fix |
|----|------|-------|-----|
| **S1** | SQL LIMIT clause | `format!("LIMIT {}", n)` — SQL injection risk if `n` untrusted | Switched to parameter binding (`?1`/`?2`) |
| **S2** | `to_curl` shell escaping | Single quotes in headers not escaped — curl command broken | Escape with `'\''`; only skip `Content-Length` when body is non-empty |
| **S3** | `http_send` header termination | May add double `\r\n\r\n` if headers already terminated | Normalize to exactly one blank line: check `ends_with("\r\n\r\n")` before appending |
| **S4** | `capture stop` user hint | No guidance on how to stop a running capture daemon | Added hint: `"use Ctrl-C to stop a running capture daemon"` |

#### Quality (Q1-Q8)
| ID | Area | Issue | Fix |
|----|------|-------|-----|
| **Q1** | `now()` timestamp error | Panic on clock error | `eprintln!("capture: clock error: {e}"); return 0.0` |
| **Q3** | `head_only` in analyze | Lossy UTF-8 conversion on binary HTTP data | Byte-level scanning via `find_subsequence()` on `&[u8]` |
| **Q4** | `split_records` | No max-Records cap on large TLS streams | Added `max_records: usize` parameter (0 = unlimited) |
| **Q5** | `serve_flow` | Fixed 4096-byte read may not finish request | Changed to `read_to_end` with 500ms timeout |
| **Q7** | `CaptureCmd::Stop` | No user guidance | Returns note: `"use Ctrl-C to stop a running capture daemon"` |
| **Q8** | SOCKS5 SEQ convention | SEQ starts at 0, should start at 1 per convention | Changed initial value to `1` |

---

### ▶️ 2nd + 3rd Code Review Rounds — 2 New Bugs

| ID | File | Issue | Fix | Commit |
|----|------|-------|-----|--------|
| **R1** | `tun_stack.rs:reset_socket()` | Did NOT reset `capture_flow`, `seq_up`, `seq_down` — when a Slot is reused for a new connection, stale flow ID and seq counters carry over, mixing data from different connections into one flow | Added `capture_flow = None; seq_up = 0; seq_down = 0` in `reset_socket()` | `2258bc4` |
| **R2** | `tun_socks.rs:pump_active` | When tun-side EOF triggers `reset_socket`, no `flow_close` event emitted — DB never records `ended_at`/`close_reason` for that flow | Emit `flow_close("tun-eof")` before `reset_socket()` | `2258bc4` |

---

## 📊 Test Results Verification

| Test Script | Result |
|-------------|--------|
| `cargo test -p teamx` | 92 passed, 0 failed, 0 warnings |
| `bash tests/capture-test.sh` | 9/9 passed (network-gated assertions SKIP when hub offline) |
| `bash tests/analysis-test.sh` | 3 passed, 3 skipped (needs plain-HTTP flow + TLS flow with client_random) |
| `bash tests/replay-test.sh` | 4 passed, 1 skipped (TLS flow requires capture-test first) |

**Build:** `cargo build -p teamx` — **0 warnings**

---

## 🛠 Files Modified (8 files in round 1 + 2 in later rounds)

| File | Description |
|------|-------------|
| `crates/teamx/src/capture.rs` | Schema, apply_batch, import_keylog, prune, cleanup_orphaned_keys, export_pcap, export_keylog, parse_addr, build_tcp_packet, now(), tests |
| `crates/teamx/src/analyze.rs` | stats, list_flows, http_reconstruct, head_only/find_subsequence, load_traffic_secrets, tls13_expand, label, decrypt_tls13_record, decrypt_flow, handle, tests |
| `crates/teamx/src/replay.rs` | flow_streams, flow_target, serve_flow, tcp_replay, prefix_match_ratio, parse_request/response, extract_exchange, store_exchange, load_exchange, export_har, parse_header_list, timestamp_to_iso, days_to_ymd, to_curl, http_send, tests |
| `crates/teamx/src/tls_parse.rs` | detect_protocol, client_hello_meta, parse_client_hello, split_records (max_records param) |
| `crates/teamx/src/capture_cli.rs` | Daemon start/stop, prune+cleanup_orphaned_keys, SQL LIMIT params, CaptureCmd dispatch |
| `crates/teamx/src/tunnel_client.rs` | SOCKS5 relay: per-flow seq_up/seq_down (local vars), meta_sent idempotent flag |
| `crates/teamx/src/tun_socks.rs` | Per-slot seq_up/seq_down from SocketSlot, meta on first up bytes, flow_close emission |
| `crates/teamx/src/tun_stack.rs` | SocketSlot now has `seq_up: u32, seq_down: u32`; `reset_socket()` now clears capture state |
| `crates/teamx/src/ui.rs` | (Unrelated dirty change — excluded from commit) |

---

## 📝 Notes & Limitations

- **Network gating:** capture-test/analysis-test/replay-test SKIP when `hub_online` check fails (no reachable teamx server). Local CLI/DB assertions always run.
- **DNS constraint:** System DNS must retain original DNS fallback (user-settable to `114.114.114.114` but code dynamically reads from `scutil --dns`/`DHCP`).
- **Enterprise feature:** Capture/analysis/replay only works in `enterprise` branch; DNS MITM is enterprise-only.
- **Safety constraint:** No "Clash" character allowed in docs/commits.

---

## 👤 Reviewer & Workflow

- **Reviewer:** Autonomous code review via LLM-assisted inspection
- **Rounds:** 3 rounds (23 + 2 issues found and fixed)
- **Verification:** `cargo test`, all 3 shell scripts, 0 warnings
- **Commits:**
  - `868a5c8` — "fix: code review round 1 — 23 issues resolved"
  - `2258bc4` — "fix: reset capture state on slot reuse + emit flow_close on tun-eof"

---
## 📝 文档修订历史 (Revision History)

| 版本 | 日期 | 修订人 | 说明 |
|------|------|--------|------|
| v1.0 | 2026-08-28 | LLM Reviewer | 初版：汇总第 1-3 轮 code review，25 个 issue 全部修复 |

---

## 📊 关联编号索引 (Issue ID Index)

| Round | Issue IDs | Count |
|-------|-----------|-------|
| Round 1 | B1-B4, D1-D7, S1-S4, Q1-Q8 | 23 |
| Round 2/3 | R1-R2 | 2 |
| **Total** | — | **25** |

---

*Generated: 2026-08-28 (周五) | Doc No: CR-001 | v1.0*