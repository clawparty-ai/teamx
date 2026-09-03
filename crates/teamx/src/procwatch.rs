//! procwatch.rs — parent-death monitoring for long-lived `teamx` processes
//! spawned by the opencode plugin (`teamx console`, `teamx serve`, ...).
//!
//! When opencode starts one of these it passes its own PID via the
//! `TEAMX_PARENT_PID` env var. A watcher thread polls that PID every couple of
//! seconds; once the parent is gone (opencode quit, crashed or was killed) the
//! process kills the children it registered here (e.g. tunnel forwards) and
//! exits, so no orphaned teamx processes are left behind.
//!
//! Without `TEAMX_PARENT_PID` the watcher does nothing (normal standalone use).
//! The liveness probe uses `libc::kill(pid, 0)`, which is Unix-only; on other
//! platforms the watcher is a no-op and children are not auto-reaped.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Child pids this process spawned that should die together with it.
static CHILDREN: OnceLock<Mutex<Vec<u32>>> = OnceLock::new();

fn children() -> &'static Mutex<Vec<u32>> {
    CHILDREN.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a child pid so it is killed when the parent of this process dies.
pub fn register_child(pid: u32) {
    #[cfg(unix)]
    children().lock().unwrap().push(pid);
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(unix)]
fn parent_alive(pid: i32) -> bool {
    // `kill(pid, 0)` reports whether the process exists (no signal sent).
    // ESRCH means the parent is gone.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Kill every registered child (SIGTERM, then SIGKILL after a short grace).
#[cfg(unix)]
pub fn kill_children_and_exit(code: i32) -> ! {
    let pids: Vec<u32> = children().lock().unwrap().clone();
    for &pid in &pids {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    for &pid in &pids {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
    std::process::exit(code);
}

/// Start the parent-death watcher (no-op unless `TEAMX_PARENT_PID` is set).
pub fn spawn_parent_monitor() {
    #[cfg(not(unix))]
    {
        let _ = Duration::from_millis(1);
        return;
    }
    #[cfg(unix)]
    {
        let Ok(ppid) = std::env::var("TEAMX_PARENT_PID") else {
            return;
        };
        let Ok(ppid) = ppid.trim().parse::<i32>() else {
            return;
        };
        if ppid <= 0 {
            return;
        }
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(2));
            if !parent_alive(ppid) {
                // opencode is gone: clean up children and exit quietly.
                kill_children_and_exit(0);
            }
        });
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn register_and_alive() {
        // Our own pid is always alive.
        assert!(parent_alive(std::process::id() as i32));
        register_child(std::process::id());
    }
}
