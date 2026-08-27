//! Makes "the pixel suite did not run" impossible to read as "the pixel suite
//! passed".
//!
//! # The trap this closes
//!
//! Every GPU test file in this directory opens with
//! `#![cfg(feature = "pixel")]`, so without that feature each one compiles to
//! **zero tests** and cargo reports `ok. 0 passed` in ~0.00s. That is
//! indistinguishable at a glance from a real green run, and it has already
//! cost the team a wrong verdict: `dialog_blur` was reported "green in 0.14s"
//! by an invocation that had in fact compiled and run nothing at all, while
//! the test was failing consistently under `--features pixel`.
//!
//! This file is deliberately **not** feature-gated, so it is the one target in
//! the pixel suite that always has something to say.
//!
//! # Two levels, because two audiences
//!
//! A human running the headless suite has done nothing wrong and must not be
//! failed for it — they get a test whose *name* states that the GPU tests were
//! skipped, which is legible in the pass list without reading counts.
//!
//! Anything that turns a run into evidence — CI, a tally, an agent reporting
//! suite health — sets `LUMA_EXPECT_PIXEL=1` and gets a hard failure instead,
//! because for that audience a feature-off run is not a weaker result, it is
//! *no result*, and silently counting it as a pass is the exact failure mode
//! above.

/// The environment variable that turns a skipped pixel suite into an error.
#[cfg(not(feature = "pixel"))]
const EXPECT: &str = "LUMA_EXPECT_PIXEL";

#[cfg(not(feature = "pixel"))]
#[test]
fn gpu_tests_were_skipped_rerun_with_features_pixel() {
    let expected = std::env::var_os(EXPECT).is_some_and(|value| value != "0");
    assert!(
        !expected,
        "{EXPECT} is set, but this run was built without `--features pixel`: \
         every GPU test file compiled to zero tests, so this run is not \
         evidence of anything. Re-run with `--features pixel`."
    );
    eprintln!(
        "note: GPU tests were skipped — this run was built without \
         `--features pixel`, so every pixel-gated file compiled to zero tests. \
         Re-run with `--features pixel --no-fail-fast` to exercise them."
    );
}

#[cfg(feature = "pixel")]
#[test]
fn gpu_tests_are_enabled() {
    // Present so the pass list says which half of the suite ran, and so the
    // guard above cannot be mistaken for a file that only exists to fail.
}
