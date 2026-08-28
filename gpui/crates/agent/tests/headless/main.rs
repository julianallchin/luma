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

mod add_tracks_empty;
mod add_tracks_error;
mod add_tracks_flow;
mod add_tracks_focus;
mod add_tracks_source_race;
mod agent_chat_track;
mod chrome_anchors;
mod dialog_escape;
mod dialog_focus;
mod empty_panel;
mod fixture_picker;
mod graph;
mod keyboard;
mod library_foundation;
mod settings;
mod shell_panels;
mod slider;
mod tab_chrome;
mod track_editor;
mod track_editor_lanes;
mod track_editor_previews;
mod track_editor_stack;
mod track_editor_strip;
mod track_editor_ux;
mod track_editor_waveform;
mod tracks;
mod venues;
mod workspace_scope;
