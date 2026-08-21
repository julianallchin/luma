//! The chat surface's palette and geometry.
//!
//! Colors come straight from [`luma_md::theme`] — one palette for the markdown
//! and the chrome around it, because a chip that disagreed with the prose it
//! sits beside would be two surfaces pretending to be one. The numbers below
//! are the chat's own, transcribed from `harness/gauntlet-chat/style-spec.md`
//! with their `file:line` sources.
//!
//! Nothing outside this crate and `luma-md` may read either. See the spec's §0:
//! the app has two surfaces now, each internally singular, and the boundary is
//! a crate boundary so a drift is a compile error.

pub use luma_md::theme::{
    card_bg, glass, glass_generation, glass_hover, grey, hairline, ink, neutral, oklch, scrim,
    wash, window_background_appearance, Theme, GLASS_ALPHA, SCRIM_ALPHA,
};

// -- geometry (theme.rs:447-473) ---------------------------------------------

/// User message bubble corner radius.
pub const BUBBLE_RADIUS: f32 = 16.0;
/// Panel and card corner radius.
pub const PANEL_RADIUS: f32 = 10.0;
/// Small control radius — buttons, chips.
pub const CONTROL_RADIUS: f32 = 6.0;

pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;

/// The panel's own header.
pub const HEADER_HEIGHT: f32 = 44.0;
/// Reserved for the working indicator, always — reserving it is what keeps the
/// composer from shifting the moment a turn starts.
pub const STATUS_STRIP_HEIGHT: f32 = 24.0;
/// Height of the gradient that fades the transcript into the panel background
/// at its bottom edge.
pub const TRANSCRIPT_FADE_BAND: f32 = 24.0;

// -- transcript (transcript.rs:56-131) ---------------------------------------

/// The reading column: 46rem. A wider panel gutters, it does not stretch prose.
pub const MAX_CONTENT_WIDTH: f32 = 736.0;
/// Between turns.
pub const GAP_TURN: f32 = 14.0;
/// Within a turn.
pub const GAP_BLOCK: f32 = 8.0;
/// `ListState` overdraw: how far past the viewport rows are measured.
pub const OVERDRAW_PX: f32 = 320.0;
/// A tool chip's height. **Declared, never measured** — a fold whose height is
/// measured makes every collapse a relayout.
pub const CHIP_HEIGHT: f32 = 38.0;
/// One line of an expanded chip's detail. Also declared: the card counts its
/// own lines and multiplies, so opening a chip is a known height change rather
/// than a measurement of a wrapped blob.
pub const CHIP_DETAIL_LINE: f32 = 16.0;
/// How many lines of one section a chip's detail shows before it is clipped.
/// A chip is a *summary* that opens — a tool that printed a thousand lines
/// still gets a card the size of a card.
pub const CHIP_DETAIL_MAX_LINES: usize = 10;
/// The disclosure chevron's box, at the chip's trailing edge.
pub const CHIP_CHEVRON: f32 = 14.0;

// -- composer (composer.rs:46-84) --------------------------------------------

/// Vertical padding inside the composer's text area.
pub const TEXTAREA_PAD_V: f32 = 20.0;
/// The text area's grow range.
pub const TEXTAREA_MIN: f32 = 76.0;
pub const TEXTAREA_MAX: f32 = 260.0;
/// The row under the text area that carries the model chip and send / stop.
pub const ACTIONS_ROW_HEIGHT: f32 = 46.0;
/// The composer plate's radius. Its own value, larger than [`PANEL_RADIUS`]:
/// the plate is the one control the eye lands on, and comet's pill reads as a
/// pill rather than as a card.
pub const COMPOSER_RADIUS: f32 = 18.0;
/// Send and stop are one circular button that changes what it holds — never
/// two buttons, so the pair can never disagree about whether a turn is running.
pub const SEND_DIAMETER: f32 = 28.0;
/// A chip in the actions row (the model name), and the empty state's prompts.
pub const CHIP_SMALL_HEIGHT: f32 = 24.0;

// -- the empty state ---------------------------------------------------------

/// The mark above the empty state's headline.
pub const HERO_GLYPH: f32 = 40.0;
/// How wide the empty state's column is allowed to grow. Narrower than the
/// panel so the hero reads as centred rather than as a full-width paragraph.
pub const HERO_WIDTH: f32 = 260.0;

// -- the panel ---------------------------------------------------------------
