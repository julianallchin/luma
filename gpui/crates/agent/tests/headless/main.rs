//! The headless suite: every outside-in test that drives the Luma app
//! through the automation harness without a GPU.
//!
//! One binary rather than one per file. Each member keeps the `#![cfg(...)]`
//! it had — an inner attribute on a `mod`-included file gates the module and
//! every test in it, exactly as it gated the standalone target — and each gets
//! its own [`support::Fixture`], which is safe here because a fixture carries
//! its library directory and motion policy in the harness `Runtime` rather
//! than in the process environment.
//!
//! Run one file's worth of tests with a filter: `cargo test --test headless tab_chrome`.

#[path = "../support/mod.rs"]
mod support;

mod add_tracks_flow;
mod library_foundation;
mod probe_view;
mod signin;
mod venue_builder;
mod venues;

mod sync_status;
