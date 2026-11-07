//! What one frame of the track editor costs while the eye is moving.
//!
//! Julian reports the timeline lagging at full zoom-out. This is that report
//! turned into a number: a track long enough and a score crowded enough to be
//! representative, zoomed all the way out, then scrolled and scrubbed
//! continuously while every frame in between is timed.
//!
//! # Why pixel mode
//!
//! Headless mode's text system is a stand-in that returns invented metrics, so
//! every string on the canvas — the bar numbers, every clip's label — shapes
//! for free there. Shaping is the single most expensive thing this canvas
//! does per glyph, so a headless number would exclude exactly the cost most
//! likely to be the answer. Pixel mode is the same deterministic platform with
//! the real text system plugged in.
//!
//! What neither mode measures is the GPU: handing the built scene to the
//! renderer has no public entry point at the pinned gpui rev (see
//! `app.timings()`). So these are CPU frame times — `run_until_parked` plus
//! the layout/prepaint/paint walk — and the budget is stated against that.
//! That is the half a timeline regression lands in: the scene this canvas
//! builds is a flat list of quads, and its cost is how many of them there are.
//!
//! # Why it is in Rust
//!
//! The width-tween measurement is
//! `tests/js/pixel/track_editor_budget.test.js`, because it is a ratio against
//! the same run. This one asserts an absolute frame budget, which `.test.js`
//! files do not do.
//!
//! # An example, not a test
//!
//! It creates a GPU device and checks a wall-clock percentile, so it is a
//! measurement, not a gate: a loaded machine would fail it for reasons that
//! have nothing to do with the code. Run it on demand:
//!
//! ```sh
//! CARGO_TARGET_DIR="$PIXEL_TARGET" cargo run -p gpui-agent --features pixel \
//!     --example track_editor_budget
//! ```
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
