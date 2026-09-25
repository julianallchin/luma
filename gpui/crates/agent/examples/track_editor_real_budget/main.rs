//! What one frame of the track editor costs on a real score, while the view
//! is moving.
//!
//! ```sh
//! LUMA_REAL_CONFIG=/path/to/a/library/copy \
//! LUMA_REAL_VENUE=Club LUMA_REAL_TRACK="Black Hole" \
//! CARGO_TARGET_DIR="$PIXEL_TARGET" cargo run -p gpui-agent --features pixel \
//!     --example track_editor_real_budget
//! ```
//!
//! # Why this exists next to `app_pixel track_editor_budget`
//!
//! The synthetic budget sweeps a *shape* — lanes, clips, one length — and
//! answers "what does a busy score cost". It cannot answer "why is this one
//! slow", because what differs between two scores is content: how many clips
//! carry a decoded preview, how short they are, how many rows a venue puts in
//! each heatmap. Only the user's own library has that, so this opens it.
//!
//! # Read-only, and why the copy is not optional
//!
//! Opening a library runs migrations, so `LUMA_REAL_CONFIG` must be a COPY.
//! See `examples/visualizer_real_score_window.rs`, which this mirrors.
//!
//! An example rather than a test: it measures and reports, and it needs a
//! library that only exists on one machine.
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
