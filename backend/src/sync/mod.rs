//! Sync: PowerSync row replication plus media transfer.
//!
//! [`schema`] names the synced tables once; [`triggers`] installs the local
//! change log and the PowerSync CRUD queue on every writer connection from
//! that list. [`connector`] talks to PowerSync Cloud and Supabase PostgREST,
//! and [`service`] owns the connection lifecycle. [`media`], [`files`] and
//! [`progress`] move audio, stems and album art, which is a separate concern
//! from records and runs on its own clock.

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
