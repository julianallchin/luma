//! The app pixel suite: outside-in tests that drive the Luma app against a
//! real renderer and read frames back as images.
//!
//! Split from `headless` because the two want opposite answers from the same
//! switch — `Harness::headless` resolves `stage_gpu` from the mode, and a
//! binary mixing both would be paying for a device in tests that never look at
//! one. See `ui_pixel` for the pixel tests that need no app at all.
//!
//! `cargo test --features pixel --test app_pixel <filter>`.

#[path = "../support/mod.rs"]
mod support;

mod account_foot_pixels;
mod add_tracks_pixels;
mod chat_context_pixels;
mod chrome_anchors_pixels;
mod dialog_host_pixels;
mod fixture_picker_pixels;
mod gauntlet;
mod gauntlet_chat;
mod graph_budget;
mod sidebar_scores_pixels;
mod sidebar_toggle_budget;
mod signin_pixels;
mod subagents_pixels;
mod tab_chrome_pixels;
mod track_editor_budget;
mod track_editor_preview_pixels;
mod track_editor_sheet_pixels;
mod track_editor_waveform_pixels;
mod venue_builder_pixels;
mod venue_patch_pixels;
mod venues_pixels;
mod visualizer;
mod visualizer_budget;
mod visualizer_capture;
mod visualizer_gizmo;
mod visualizer_live;
mod visualizer_playback_budget;
mod visualizer_playback_soak;
mod visualizer_pointer;
mod visualizer_present_budget;
mod visualizer_zoom_budget;
