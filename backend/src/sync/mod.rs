//! Sync: media transfer today, PowerSync row replication next.
//!
//! [`schema`] names the synced tables once; [`triggers`] installs the local
//! change log on every writer connection from that list. [`files`] moves audio,
//! stems and album art, which is a separate concern from records and stays
//! whatever the record transport is.

pub mod error;
// Media transfer has no transport until phase two reconnects it; the code is
// kept whole rather than rewritten from memory later.
#[allow(dead_code)]
pub mod files;
#[allow(dead_code)]
pub mod host;
#[allow(dead_code)]
pub mod progress;
pub mod schema;
#[allow(dead_code)]
pub mod traits;
pub mod triggers;
