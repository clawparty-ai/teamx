//! buildinfo.rs — build metadata exposed by `teamx version`.
//!
//! Values are stamped at compile time by `build.rs`:
//! - commit: git short SHA of the build (or `unknown` for source snapshots);
//! - built_at: UTC build time (epoch seconds, formatted on read).

use serde_json::{json, Value};

/// The git commit (short SHA) this binary was built from.
pub fn commit() -> &'static str {
    option_env!("TEAMX_BUILD_COMMIT").unwrap_or("unknown")
}

/// UTC build time as epoch seconds (0 when unavailable).
pub fn built_at_epoch() -> i64 {
    option_env!("TEAMX_BUILD_TIME_EPOCH")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

/// Human-readable UTC build time.
pub fn built_at_iso() -> String {
    let ts = chrono::DateTime::from_timestamp(built_at_epoch(), 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap());
    ts.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// JSON form (also what `teamx version --json` prints).
pub fn json_value() -> Value {
    json!({
        "version": version(),
        "commit": commit(),
        "built_at": built_at_iso(),
        "built_at_epoch": built_at_epoch(),
    })
}

/// Human-readable multi-line form (default `teamx version` output).
pub fn human() -> String {
    format!(
        "teamx {version}\ncommit {commit}\nbuilt {built_at}\n",
        version = version(),
        commit = commit(),
        built_at = built_at_iso(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_output_shape() {
        let v = json_value();
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert!(v["commit"].is_string());
        assert!(v["built_at"].is_string());
        assert!(built_at_epoch() > 0, "build time should be stamped");
    }

    #[test]
    fn human_includes_commit_and_built() {
        let h = human();
        assert!(h.contains("commit "));
        assert!(h.contains("built "));
    }
}
