# 16 — Code Review Report: Transparent Proxy & DNS (2026-08-24)

> Scope: full-project review with emphasis on the transparent-proxy / local-DNS
> work introduced in `7118e8c` and the follow-up fixes. Three review rounds
> were run; issues found were fixed in the same round and re-reviewed until no
> new findings appeared.
>
> Commits: `2601e14` (DNS fallback), `dccae8e` (round-1 fixes),
> `607a729` (round-2 fixes).

## Review Method

Each round walked a different slice of the code with a different lens:

1. **Round 1 — architecture & correctness**: data flow of a DNS query and a TCP
   connection through dns_proxy → server RPC → exit → tun0 smoltcp → bridge;
   resource lifetime (waiters, pipes, allocations).
2. **Round 2 — security & boundaries**: symlink/TOCTOU on fixed paths, DNS
   poisoning surfaces (A vs AAAA), unbounded growth, input validation.
3. **Round 3 — convergence**: re-read every fix, sweep remaining modules
   (metrics, db migrations, table views, app entry) for anything missed.

## Findings & Fixes

### Round 1 — architecture & correctness

| ID | Severity | File(s) | Finding | Fix |
|---|---|---|---|---|
| H1 | High | `dns_proxy.rs`, `tunnel_client.rs` | No DNS cache: every query for an intercepted domain paid a fresh tokio runtime + mTLS handshake to the server (~1 s). | 60 s TTL in-process cache in `dns_proxy`; repeated queries are served from memory. |
| H3 | High | `serve.rs` | `team.resolve_dns` registered a oneshot waiter per request; if the exit never replied (6 s timeout) the waiter stayed in `resolve_waiters` forever — slow memory leak. | On timeout/error the waiter is completed-and-dropped (`complete_resolve(sid, [])`). |
| H4 | High | `TeamxCore.run` (Swift) | Classic pipe deadlock: `waitUntilExit()` before draining stdout/stderr. Any CLI output > 64 KB (large route table) would hang the app. | Read both pipes to EOF first, then wait for exit. |
| H5 | High | `ControlPanelController.refreshConnection` | Panel refreshed every 2 s with three synchronous mTLS curl calls (up to 5–8 s each) on the main thread — up to ~20 s UI freeze when the server was slow. | Network calls moved to a background queue; only the UI update hops back to the main thread. |
| M2 | Med | `tun_stack.rs` | `TxToken::consume` heap-allocated a fresh buffer for every emitted packet (SYN-ACKs, data — high frequency). | Reused scratch buffer in `TunPhy` (split-borrowed alongside rx_buf/tun). |
| M3 | Med | `tun_stack.rs` | ICMPv4 checksum left at "ignored" while IPv4/UDP/TCP were TX-computed; emitted ICMP would carry checksum 0. | `icmpv4 = Checksum::Tx`. |
| L1 | Low | `tun_socks.rs` | CIDR network routes added twice at startup (caller + `ip_route_loop`) producing duplicate "File exists" noise. | Removed from `ip_route_loop` (caller adds them once). |

### Round 2 — security & boundaries

| ID | Severity | File(s) | Finding | Fix |
|---|---|---|---|---|
| F1 | **High (security)** | `Privileged.swift`, `gui_panel.rs`, `TeamxCore.swift` | The elevated (`root`) process wrote its log to the fixed path `/tmp/teamx-tun0.log`. `/tmp` is world-writable: any local user could pre-create it as a symlink to e.g. `/etc/passwd`; the shell redirection follows symlinks, so the root process would truncate/overwrite an arbitrary file (CWE-61). | Log moved to `$TEAMX_HOME/tun0.log` (exported into the elevated shell; directory created with `mkdir -p`). The panel's log tail reads the same location via `NSHomeDirectory() + "/.teamx/tun0.log"`. |
| F2 | High | `dns_proxy.rs` | When the server is unreachable, each intercepted-domain query blocked for the full ~15 s RPC timeout, serially, with no negative caching — total DNS stall while the client waits and then falls back. | Failed resolutions are cached too (10 s TTL; successes 60 s), so a dead server costs at most one timeout per distinct domain per 10 s window. |
| F2b | High | `dns_proxy.rs` | AAAA queries for intercepted domains were forwarded to the upstream (censored) resolver, returning poisoned IPv6 addresses; apps preferring IPv6 would bypass the proxy and fail. | Non-A queries for intercepted domains now get an **empty NOERROR** answer (`build_empty_response`); clients fall back to the proxied real-IP A record. Non-intercepted domains still forward upstream. |
| F3 | Med | `dns_proxy.rs` | `cache` and shared `ip_map` grew without bound over a long session. | Caps: cache trimmed when ≥1024 entries (expired dropped); ip_map cleared at ≥8192. |
| UX1 | Low | `DraggableTable.swift` | Table resized on any drag anywhere (not just from the handle), and wrote UserDefaults on every mouse-dragged frame. | Dragging only starts if the handle was pressed; height persisted once on `mouseUp`. |

### Round 3 — convergence

Re-read all fixes; swept `metrics.rs`, DB migrations, `SimpleTable`,
`main.swift`, and the resolve authorization path:

- `team.resolve_dns` resolves exits only inside the caller's own teams
  (`registry.get(team_id, name)` + `teams_for_member`) — no cross-team access.
- `build_dns_response` packet-length arithmetic cannot overflow (payload capped
  by the 2048 B socket buffer).
- Remaining accepted items (documented below).

## Accepted / Deferred Items

These were found but deliberately not changed now:

| Item | Why deferred |
|---|---|
| Main poll loop wakes every 2 ms even when idle (soft busy-loop). | Correct fix is `tokio::AsyncFd` waiting on the tun fd; requires restructuring the non-Send single-thread runtime. Current CPU cost is small. |
| `open_tunnel_bridge(...).await` runs inline in the tun0 main loop; a hung server connection would stall all connections until it times out. | Architectural (single-threaded smoltcp stack is not `Send`). Mitigated by the HTTPS 15 s cap; a proper fix needs a spawn + channel redesign. |
| Several CDN domains share one IP; the `ip -> domain` map keeps the last writer, so TLS SNI may use a sibling domain (e.g. dials `google.com` for a `googleapis.com` IP). | Google's edge accepts any of its own names; impact not observed. A real fix would need SNI passthrough from the client hello. |
| `MetricsRegistry::snapshot` resets counters, so two concurrent snapshots split the measured bytes between them. | Snapshot callers are effectively serialized today; low impact. |
| `resolve_dns` blocks the single DNS thread for up to 15 s when the server is unreachable. | With the negative cache this costs one block per domain per 10 s; full fix overlaps the AsyncFd/threading refactor above. |
| `mTLSEnvPrefix` builds a shell string from environment values (single-quote escaped, double-quote escaped again for AppleScript). Values come from the user's own environment, so this is self-inflicted-input only, but a helper binary would be cleaner than string-built elevation. | Refactor risk outweighs benefit for now. |

## Verification After Fixes

```
cargo build -p teamx   → 0 warnings
cargo test -p teamx    → 73 passed (incl. new build_a_response answer-count test)
swift build            → 0 warnings
```

End-to-end (system DNS = `127.0.0.1` + original DNS fallback):

- `dig www.google.com` → ANSWER: 8, real Google IPs (via exit)
- `curl https://www.google.com/generate_204` → 204
- `curl https://www.baidu.com` → 200 (non-intercepted, direct)
- AAAA for intercepted domains returns NOERROR/NODATA (no poisoned IPv6)

## Follow-ups (future work)

1. Watchdog/auto-restore if the tun0 process dies while system DNS points at
   `127.0.0.1` (currently documented as a known limitation).
2. AsyncFd-based tun readiness to remove the 2 ms poll.
3. Bridge tasks spawned off the main loop so a hung exit cannot stall tun0.
