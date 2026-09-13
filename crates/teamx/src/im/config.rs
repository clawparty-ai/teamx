//! IM configuration.
//!
//! Keys are graded (design §5.4): the gateway holds the provider `app_secret`;
//! `serve` must **not**. Config is read from the environment so no secret is
//! ever written to disk by the core. `dry_run` defaults to **true** so an
//! unconfigured deployment never accidentally sends real messages.

/// IM runtime configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImConfig {
    pub feishu_app_id: Option<String>,
    pub feishu_app_secret: Option<String>,
    /// When true, providers log instead of sending (safe default).
    pub dry_run: bool,
}

impl ImConfig {
    /// Read config from the environment.
    ///
    /// Env: `TEAMX_IM_FEISHU_APP_ID`, `TEAMX_IM_FEISHU_APP_SECRET`,
    /// `TEAMX_IM_DRY_RUN` (`1`/`true` enables; anything else disables).
    /// `dry_run` defaults to true when the env var is absent.
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        let dry_run = match get("TEAMX_IM_DRY_RUN") {
            Some(v) => v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"),
            None => true,
        };
        Self {
            feishu_app_id: get("TEAMX_IM_FEISHU_APP_ID"),
            feishu_app_secret: get("TEAMX_IM_FEISHU_APP_SECRET"),
            dry_run,
        }
    }

    /// Whether feishu credentials are present (required for live sends).
    pub fn feishu_configured(&self) -> bool {
        self.feishu_app_id.is_some() && self.feishu_app_secret.is_some()
    }
}
