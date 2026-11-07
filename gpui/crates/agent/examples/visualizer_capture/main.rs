//! Capture the 3D view's shots, and time its frames.
//!
//! ```sh
//! CARGO_TARGET_DIR="$PIXEL_TARGET" cargo run --release -p gpui-agent --features pixel \
//!     --example visualizer_capture
//! ```
//!
//! An example, not a test, because it asserts nothing: it is the eye and the
//! stopwatch, run when the viewport's look or its cost is the question. The
//! frames are copied into `<shots>/visualizer` (`LUMA_SHOTS`, else
//! `<temp>/luma-shots`).
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
