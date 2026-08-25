# 17 — tun0 Improvements: Watchdog / AsyncFd / Async Bridge

> Status: **design proposal (to be implemented after confirmation)**
> Related: `docs/09-design-tun0.en.md`, `docs/15-design-transparent-proxy.en.md`,
> `docs/16-code-review-transparent-proxy.en.md`
> Date: 2026-08-24

This document analyzes the three structural issues flagged as
"accepted/deferred" in the code review, with root causes, quantified impact,
option comparison and a recommendation. **Implementation starts after
confirmation.**

---

## Issue 1 — Watchdog: DNS self-healing after tun0 death

### Current state & risk

`tun0 start` sets system DNS to `[127.0.0.1, <original DNS>]`:

- tun0 running normally: the local DNS proxy answers (intercepted domains are
  resolved via the exit).
- tun0 **exits normally** (panel stop / `teamx tun0 stop`):
  `restore_system_dns()` restores from `~/.teamx/dns-backup.json` — fine.
- tun0 **dies abnormally** (crash, `kill -9`, force quit): nobody restores the
  DNS. Consequences:
  - Every resolution first waits for `127.0.0.1` to time out (macOS
    mDNSResponder penalty window ≈ 3–5 s per dead server), then falls to the
    second server.
  - The result is not a full outage but **every resolution being 3–5 s slower**;
    timeout-sensitive tools (CLIs with short hardcoded timeouts) fail outright.
  - Worse case: the process is "half-dead" (alive but the main loop is stuck) —
    the local proxy port still exists but never answers; a watchdog that only
    checks "process exists" would wrongly report healthy.

### Goal

Restore system DNS automatically within **≤10 s** of an abnormal tun0 death,
with zero false restores while healthy.

### Options

| Option | Approach | Pros | Cons |
|---|---|---|---|
| W-A in-process watchdog thread | A thread inside the tun0 process holds a copy of the DNS backup; the main loop feeds it every second; if unfed for 2 s it restores DNS as root and deletes the backup | Self-contained | Only covers process death; cannot handle "alive but stuck"; `kill -9` kills the watchdog thread too — **ineffective** |
| W-B standalone launchd daemon | Registering a one-shot LaunchAgent (`teamx-dns-watchdog`) alongside tun0 start; it polls `pgrep teamx tun0`; when the process is gone and a backup file exists → restore + self-unload | Decoupled from the tun0 process; survives kill -9; native macOS mechanism | One more resident component; install/cleanup logic; Linux needs a systemd-run variant |
| W-C heartbeat-file probing | tun0 touches `~/.teamx/tun0.alive` every second; a lightweight resident watcher (could be the GUI app) detects a stale heartbeat → prompts or auto-restores | Simple | Auto-restore depends on a third party (the GUI) being alive; degrades to a hint in CLI-only setups |
| W-D heal-on-start (recommended combo) | (1) Before each `tun0 start` / `dns_proxy spawn`: if "backup exists but pgrep finds no tun0 process" → restore the leftover backup first; (2) register `restore_system_dns()` in signal handlers (SIGTERM/SIGINT), plus a self-destruct guard in dns_proxy ("N consecutive resolve failures → restore DNS and exit") | Covers the most common case (leftovers from a previous crash); small implementation | Doesn't cover mid-run stalls |

### Recommendation

**W-D now, W-B later**, in two phases:
- Phase 1 (~80 lines): W-D heal-on-start + signal-handler restore + dns_proxy
  self-destruct guard.
- Phase 2 (optional): W-B standalone LaunchAgent covering `kill -9`.

---

## Issue 2 — AsyncFd: removing the 2 ms busy-poll in the main loop

### Current state & impact

```rust
loop {
    stack.poll();                       // read tun fd + drive smoltcp + UDP DNS
    /* take_new_connection / pump_active */
    tokio::time::sleep(2ms).await;      // yield to the scheduler
}
```

- Idle CPU usage measured at ~0.5–2% (noticeable on laptop battery/thermals).
- Under load it doesn't matter (poll must run anyway); **idle time is pure
  waste**.
- We can't simply go back to synchronous `phy::wait(fd, timeout)`: it blocks
  the current-thread runtime and starves the bridge spawn tasks on the same
  thread (a lesson already learned).

### Root cause

smoltcp's `Interface::poll` wants to run "whenever there's a chance", but we
only want to run when **the tun fd is readable** or **a timer fires**
(retransmits / TIME_WAIT maintenance). We need an async primitive that waits on
both without blocking the runtime.

### Options

| Option | Approach | Pros | Cons |
|---|---|---|---|
| A1 tokio AsyncFd (recommended) | Wrap the tun fd in `AsyncFd`; main loop does `tokio::select! { _ = asyncfd.readable(), _ = sleep(next_timer) }` then polls | Event-driven, 0% idle CPU; keeps the single-thread model; change concentrated in run_tun_proxy (~40 lines) | fd must be non-blocking (already is); readable is edge-triggered-ish and needs `clear_ready()`; must re-poll immediately after writes |
| A2 reader thread + channel | std::thread blocks reading the tun fd and forwards packets over an unbounded channel | Simple and intuitive | Extra per-packet cross-thread copy; big TunPhy refactor (breaks the "phy reads the fd directly" assumption); backpressure is hard at high throughput |
| A3 adaptive polling | Dynamically adjust sleep: 50 ms idle → 2 ms under load | Smallest change (~10 lines) | Still polling; adds latency jitter (first packet may wait up to 50 ms); treats symptoms only |
| A4 waker injection into phy | Register a waker so fd-readability wakes the loop directly | Most elegant | smoltcp 0.14's phy trait has no waker hook; requires another vendor patch; highest complexity |

### Recommendation

**A1**. Key points:
- `let mut async_fd = tokio::io::unix::AsyncFd::new(tun_fd)?;`
- select branch: `(async_fd.readable().await)?.clear_ready();` → `stack.poll()`
- timer branch: `sleep_until(next_maintenance)` (smoltcp exposes no explicit
  interface; a fixed 100 ms floor satisfies retransmit precision)
- Regression: bidirectional throughput must not regress (existing curl google
  204 case) + idle CPU sampling comparison.

---

## Issue 3 — Async bridge: connection setup no longer stalls the main loop

### Current state & impact

```rust
while let Some((handle, remote)) = stack.take_new_connection() {
    let bridge = open_tunnel_bridge(...).await;   // ← inline await in the main loop
```

`open_tunnel_bridge` = new mTLS WS connection + connect frame + wait for
stream_open. ~300 ms when healthy, up to 15 s when the server/network is slow.

**During this await the main loop doesn't run at all**: data pumping for
existing connections stops (an in-flight page load freezes), new SYNs aren't
processed, UDP DNS (fake-dns mode) goes silent. Opening a page with dozens of
connections serializes bridge setup — seconds of perceptible stutter.

Secondary risk: a failed bridge resets that socket, but the slot state machine
(Connecting→Active) is coupled to the same loop, so any slowness amplifies.

### Goal

Bridge setup (and retries) never block the data pump; multiple bridges set up
concurrently.

### Options

| Option | Approach | Pros | Cons |
|---|---|---|---|
| B1 spawn + oneshot backfill (recommended) | Main loop marks Connecting → `tokio::spawn`s `open_tunnel_bridge`; the result comes back over a oneshot/mpsc and the main loop attaches it to the slot on the next poll | Main loop never blocks on setup; concurrent setup for free; change concentrated in tun_socks.rs (~60 lines) | The task needs clones of server_url/exit etc.; must handle "result arrives after the slot was reset" (generation counter or state==Connecting check) |
| B2 queue + single worker | A dedicated worker task sets up bridges serially from a queue; main loop only enqueues | Main loop unblocked; worker can back off/retry | Still serial setup (slow page bring-up); extra queue state management |
| B3 connection pool pre-warm | Keep N WS connections to the server warm; setting up a bridge is just sending a frame | Setup latency drops to ~RTT | Major tunnel protocol/server changes; out of scope |

### Recommendation

**B1**. Key points:
- Slot gains `generation: u64` (incremented by every `reset_socket`); the spawn
  task captures `(handle, generation)`; the main loop accepts the result only
  if the generation still matches.
- Result channel:
  `mpsc::unbounded_channel<(SocketHandle, u64, Result<TunnelBridge, String>)>`.
- Failure path: the spawn task must NOT touch the non-Send stack directly;
  send the error over the result channel and let the main loop reset the slot.
- Concurrency cap: defer spawning when ≥8 slots are Connecting (server overload
  protection); queue for the next round.

---

## Suggested Implementation Order

Independent of each other; ordered by benefit/risk:

1. **Issue 1 phase 1 (watchdog heal-on-start + signals + self-destruct)** — small
   change, removes the most user-visible failure.
2. **Issue 3 (B1 async bridges)** — removes perceptible stutter; medium change,
   focus tests on generation races.
3. **Issue 2 (A1 AsyncFd)** — battery/CPU win; same code region as #3, do it
   after 1/3 stabilize to avoid merge friction.

Estimated effort: phase 1 ≈ half a day; B1 ≈ 1 day incl. regression; A1 ≈ half
a day incl. CPU verification.

## Regression Checklist (run once all landed)

- [ ] curl google 204 (main transparent-proxy path)
- [ ] Multi-connection page loads without stall; existing downloads keep
      flowing while bridges set up
- [ ] `kill -9` tun0 → DNS restored within ≤10 s (watchdog)
- [ ] Normal tun0 stop → DNS restored immediately
- [ ] Idle CPU ≈ 0% over 1 minute (AsyncFd)
- [ ] 73 cargo tests + zero-warning Swift build
