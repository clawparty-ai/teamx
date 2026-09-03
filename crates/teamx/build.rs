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
    println!("cargo:rerun-if-changed=build.rs");
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

    // 2. Build time as UTC epoch seconds; formatted at runtime with chrono.
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=TEAMX_BUILD_TIME_EPOCH={epoch}");
}
