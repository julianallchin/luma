//! The widget pixel suite: `luma-ui` surfaces under a real renderer, with no
//! Luma app and no library behind them.
//!
//! Separate from `app_pixel` because these build their own root and their own
//! motion policy — the morph proof runs at 10x so a burst can sample a 150ms
//! curve per frame — and because nothing here needs a seeded database.
//!
//! `cargo test --features pixel --test ui_pixel <filter>`.

#[path = "../support/mod.rs"]
mod support;

mod dialog_blur;
mod dialog_morph;
mod pixel;
mod shell_motion;
