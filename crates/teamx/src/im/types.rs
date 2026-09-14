//! Shared IM types: delegated context + message/event primitives.
//!
//! The delegated context is what an inbound IM action carries across the
//! gateway boundary. It **never carries a `member_id`** — the server resolves
//! the teamx member from the binding table (`im_bindings`), so a compromised
//! gateway cannot impersonate a member (design §5.3, identity B).

/// Context delegated from the IM gateway to the server for one inbound action.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImDelegatedContext {
    /// Provider key, e.g. `"feishu"`.
    pub platform: String,
    /// Provider-side user id (open_id / user_id).
    pub im_user_id: String,
    /// Provider-side tenant / team id (optional).
    pub im_team: Option<String>,
    /// Provider signature over the delegated fields (verified by the gateway).
    pub signature: Option<String>,
    /// Unix epoch seconds when the delegation was minted (±5min freshness).
    pub timestamp: i64,
    /// One-time nonce — the inbound idempotency key (`im_seen`, replay →
    /// rejected). Empty for legacy/CLI-constructed contexts (no gateway).
    pub nonce: String,
}

/// Where an outbound message goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImChannel {
    /// A group / chat id (broadcast).
    Group(String),
    /// A single IM user (direct message).
    Direct(String),
}

/// An outbound message rendered by a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImMessage {
    pub channel: ImChannel,
    pub title: Option<String>,
    pub body: String,
    /// Render as a card / markdown block instead of plain text.
    pub markdown: bool,
}

/// A normalized inbound event parsed from a provider payload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImEvent {
    pub platform: String,
    /// Provider message id — the inbound idempotency key (`im_seen`).
    pub im_msg_id: String,
    pub im_user_id: String,
    pub text: String,
}
