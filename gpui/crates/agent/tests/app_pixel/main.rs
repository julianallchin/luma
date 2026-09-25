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

mod add_tracks_pixels;
mod pixel_suite_guard;
mod track_editor_budget;
mod track_editor_waveform_pixels;
mod venues_pixels;
mod visualizer_capture;
mod visualizer_playback_soak;
