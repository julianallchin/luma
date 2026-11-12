//! Row replication through PowerSync, and media transfer beside it.
//!
//! [`schema`] names the synced tables once and [`triggers`] installs the upload
//! queue from that list. [`connector`] talks to PowerSync
//! Cloud and Supabase PostgREST; [`service`] owns the connection lifecycle.
//! [`media`], [`files`] and [`progress`] move bytes on their own clock, through
//! the Storage client in [`supabase`].

pub(crate) mod audio;
pub mod connector;
pub mod error;
pub mod files;
mod legacy_audio;
pub mod media;
pub mod progress;
pub mod schema;
pub mod service;
pub mod supabase;
pub mod triggers;

#[cfg(test)]
mod powersync_tests;
#[cfg(test)]
mod two_device_tests;
