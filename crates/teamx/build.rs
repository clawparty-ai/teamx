//! build.rs — inject build metadata for `teamx version` (commit + build time).
//!
//! The commit is resolved from the enclosing git repository when available;
//! otherwise (source snapshots, e.g. a copy rsync'd to a build host without
//! `.git`) it falls back to the `TEAMX_BUILD_COMMIT` environment variable so
//! reproducible builds can still stamp the exact commit.

use std::env;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // NOTE: deliberately no `cargo:rerun-if-changed=build.rs` here — emitting
    // any rerun-if-changed disables cargo's default "rerun when a package file
    // changes", which would freeze the commit/time stamp at whatever it was the
    // last time build.rs itself was touched (deployed binaries then report a
    // stale commit and cannot be verified with `teamx version`).
    println!("cargo:rerun-if-env-changed=TEAMX_BUILD_COMMIT");

    // 1. Commit: explicit env override > enclosing git repo > "unknown".
    let mut commit = env::var("TEAMX_BUILD_COMMIT").unwrap_or_default();
    if commit.is_empty() {
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        // `crates/teamx` is the package dir; the repo root is its parent.
        for probe in [manifest_dir.clone(), format!("{manifest_dir}/..")] {
            if let Ok(out) = Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .current_dir(&probe)
                .output()
            {
                if out.status.success() {
                    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !s.is_empty() {
                        commit = s;
                        break;
                    }
                }
            }
        }
    }
    if commit.is_empty() {
        commit = "unknown".to_string();
    }
    println!("cargo:rustc-env=TEAMX_BUILD_COMMIT={commit}");

    // 2. Build time: UTC epoch (for the JSON form) + a formatted ISO string
    //    (for the built-in `teamx --version` line, stamped at compile time).
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=TEAMX_BUILD_TIME_EPOCH={epoch}");
    println!("cargo:rustc-env=TEAMX_BUILD_TIME={}", format_utc(epoch));
}

/// Format unix epoch seconds as UTC `YYYY-MM-DDTHH:MM:SSZ` (no chrono needed
/// inside the build script; this is the Howard Hinnant civil-from-days
/// algorithm).
fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let ss = rem % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Convert days since 1970-01-01 to a (year, month, day) civil date.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}
