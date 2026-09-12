//! Row replication through PowerSync, and media transfer beside it.
//!
//! [`schema`] names the synced tables once and [`triggers`] installs the change
//! log and the upload queue from that list. [`connector`] talks to PowerSync
//! Cloud and Supabase PostgREST; [`service`] owns the connection lifecycle.
//! [`media`], [`files`] and [`progress`] move bytes on their own clock.

pub mod connector;
pub mod error;
pub mod files;
pub mod host;
pub mod media;
pub mod progress;
pub mod schema;
pub mod service;
pub mod triggers;

#[cfg(test)]
mod powersync_tests;
#[cfg(test)]
mod two_device_tests;
