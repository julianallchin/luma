//! The user's repro: a track playing, then zooming in, and where the frame goes.
//!
//! ```sh
//! CARGO_TARGET_DIR=target-pixel cargo run -p gpui-agent --features pixel \
//!     --example visualizer_playback_zoom_repro
//! ```
//!
//! # Why another stage measurement
//!
//! `visualizer_playback_budget` measures playback and `visualizer_zoom_budget`
//! measures zooming, each on its own. The reported failure is *both at once* —
//! "playing a track lags hella, unplayable; zooming in freezes" — and the two
//! costs are not independent. Playback already pays the score, the frame
//! assembly and the hit-test rebuild once per frame; a wheel gesture multiplies
//! how many times per displayed frame that happens. A test that never does both
//! cannot see the product.
//!
//! # An example, not a test
//!
//! The isolated renderer profile says every one of these cases is inside
//! budget, and the user's hands say otherwise, so the useful output here is an
//! *attribution* and not a pass. It has no same-run gate, so it is not a test.
//! The one thing it does check is that the rig under measurement is the one it
//! claims — every number is meaningless if the stage quietly fell back to four
//! movers or an unlit scene.
//!
//! # Reading the output
//!
//! Four numbers, and the point is which of them moves:
//!
//! - `drawMs` — gpui's element walk on the **UI thread**. `sample`, `build` and
//!   `pick` all happen inside it, so it bounds them.
//! - `parkedMs` — the app settling its async work.
//! - `UI (S/B/P)` — that walk split into score evaluation, frame assembly and
//!   hit-test rebuild.
//! - `PRES` — wall time between frames actually reaching the screen.
//! - `gpu` — the renderer thread's own half: CPU encode, GPU pass total and
//!   cluster binning, read off the frame-stats panel. This is the only one of the
//!   five that zooming can move, because zooming changes fill and nothing else.
//!
//! A UI-thread stall shows as `drawMs` rising with `PRES`. A renderer that
//! cannot keep up shows as `PRES` rising while `drawMs` stays flat. A gesture
//! storm shows as neither rising much while the *count* of renders per gesture
//! explodes, which is why `frames` is reported and not just the percentiles.
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
