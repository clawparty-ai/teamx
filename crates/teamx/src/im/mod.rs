//! IM access — the **control plane** for remote team operation & management.
//!
//! Layout (locked by `docs/design/im-access-requirements.md` §5.4):
//! `im/{config,types,providers/*}` with `gateway` / `notifier` to follow.
//!
//! Phase-1 (P1) lands the **provider seam** + the feishu primitives so the
//! binding / notifier / read-only / gateway tasks can build on a stable base.
//! IM is control-plane only: it never executes code or tasks (that stays in
//! opencode/dsh clients).

pub mod config;
pub mod notifier;
pub mod providers;
pub mod types;

pub use config::ImConfig;
pub use notifier::Notifier;
pub use providers::feishu::FeishuProvider;
pub use providers::ImProvider;
pub use types::{ImChannel, ImDelegatedContext, ImEvent, ImMessage};
