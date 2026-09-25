//! The session fixture lives in the library now (`gpui_agent::fixture::session`)
//! so the script runner seeds the same way. This file stays because
//! `support/chat.rs` includes it by path.

#[allow(unused_imports)] // each binary uses a different subset
pub use gpui_agent::fixture::session::*;
