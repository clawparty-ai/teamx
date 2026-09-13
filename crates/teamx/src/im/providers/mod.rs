//! IM providers — the seam between teamx and an IM platform.
//!
//! `ImProvider` captures the four provider responsibilities (design §5.4):
//! **receive events / send messages / verify signatures / resolve identity**.
//! Phase-1 lands only the feishu implementation; a second platform is added by
//! implementing this trait (no core changes) — the "second implementation
//! reverse-extracts the trait" rule.

pub mod feishu;

use crate::im::types::{ImDelegatedContext, ImEvent, ImMessage};

/// A pluggable IM platform adapter.
pub trait ImProvider {
    /// Provider key, e.g. `"feishu"`.
    fn platform(&self) -> &'static str;

    /// Send a message (honours dry-run + rate limits). Must not block the
    /// server on platform failure.
    fn send(&self, msg: &ImMessage) -> Result<(), String>;

    /// Verify an inbound callback's signature + timestamp/nonce (replay-safe).
    fn verify(&self, ctx: &ImDelegatedContext) -> Result<(), String>;

    /// Parse a raw provider payload into a normalized [`ImEvent`].
    fn parse_event(&self, raw: &str) -> Result<ImEvent, String>;

    /// Resolve the provider-side identity from a delegated context.
    fn resolve_identity(&self, ctx: &ImDelegatedContext) -> Result<String, String>;
}
