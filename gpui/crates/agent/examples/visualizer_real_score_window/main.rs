//! A real score, played through the real renderer, sampled second by second.
//!
//! ```sh
//! LUMA_REAL_CONFIG=/path/to/a/library/copy \
//! LUMA_REAL_VENUE=Club LUMA_REAL_TRACK="…" \
//! LUMA_REAL_FROM=45 LUMA_REAL_TO=53 \
//! CARGO_TARGET_DIR=…/target-pixel cargo run -p gpui-agent --features pixel \
//!     --example visualizer_real_score_window -- [seconds|zoom]
//! ```
//!
//! # Why this exists next to the synthetic instruments
//!
//! `app_pixel visualizer_playback_zoom_repro` sweeps a *shape* — rig size, clip count —
//! and answers "what does this configuration cost". It cannot answer "why is
//! this show slow at 0:49", because the thing that changes at 0:49 is content:
//! which fixtures a clip selects, and what kind of fixtures those are. Only the
//! user's own library has that.
//!
//! # Read-only, and why the copy is not optional
//!
//! Opening a library runs migrations, so pointing this at a live one would
//! write to it. `LUMA_REAL_CONFIG` must be a COPY. Nothing here writes to the
//! library either way, but the app underneath it does not promise that.
//!
//! # An example, not a test
//!
//! It needs a library that only exists on one machine, so it is not a gate. It
//! reports; it asserts only that it measured the rig it claims to have measured.
//! `seconds` (the default) samples each second of the window; `zoom` dollies
//! into the beams while the track plays.
//!
//! Needs `--features pixel`; without it this binary only says so.

#[cfg(feature = "pixel")]
mod measure;

fn main() {
    #[cfg(feature = "pixel")]
    measure::main();
    #[cfg(not(feature = "pixel"))]
    eprintln!("{} needs `--features pixel`", env!("CARGO_CRATE_NAME"));
}
