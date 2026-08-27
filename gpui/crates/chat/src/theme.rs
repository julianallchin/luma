//! The chat surface's palette and geometry.
//!
//! Colors come straight from [`luma_md::theme`] — one palette for the markdown
//! and the chrome around it, because a chip that disagreed with the prose it
//! sits beside would be two surfaces pretending to be one. That palette in
//! turn names roles over [`luma_ui::glass`], which is the grey ladder at a
//! coverage, so there is one chain of definitions and no crate on it mints a
//! tone. The numbers below are the chat's own, transcribed from
//! `harness/gauntlet-chat/style-spec.md` with their `file:line` sources.
//!
//! This module re-exports rather than redefines: a token spelled twice is the
//! only way the chat and the shell could come to disagree about a surface they
//! share.

pub use luma_md::theme::{
    card_bg, glass, glass_generation, glass_hover, hairline, ink, neutral, oklch, overlay, panel,
    panel_opaque, scrim, wash, window_background_appearance, Theme, GLASS_ALPHA, SCRIM_ALPHA,
};

// -- geometry (theme.rs:447-473) ---------------------------------------------

// Corners come from `luma_ui::radius` — the one ladder — not from constants
// here: a radius spelled twice is the only way the chat and the shell could
// come to disagree about a corner.

/// How much of the reading column a user bubble may take. Short of the whole
/// column on purpose: the ragged right edge is what says "this one is yours",
/// and a bubble at full width reads as another assistant block.
pub const BUBBLE_WIDTH_SHARE: f32 = 0.8;

pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;

/// The panel's own header.
pub const HEADER_HEIGHT: f32 = 44.0;
/// An icon button in that header — rewind, new chat.
pub const HEADER_BUTTON: f32 = 24.0;
/// Reserved for the working indicator, always — reserving it is what keeps the
/// composer from shifting the moment a turn starts.
pub const STATUS_STRIP_HEIGHT: f32 = 24.0;
/// Height of the gradient that fades the transcript into the panel background
/// at its bottom edge.
pub const TRANSCRIPT_FADE_BAND: f32 = 24.0;

// -- transcript (transcript.rs:56-131) ---------------------------------------

/// The reading column: 46rem. A wider pane gutters, it does not stretch prose.
pub const MAX_CONTENT_WIDTH: f32 = 736.0;
/// The column's minimum gutters, either side — comet's 48px. The turn rail
/// lives inside the left one.
pub const CONTENT_GUTTER: f32 = 48.0;
// The spacing rhythm, in priority order — three gaps and nothing else, which
// is what keeps a transcript from reading as a pile of differently-spaced
// cards. A block's gap is decided by what it sits *next to*, never by what it
// is.
/// A new turn begins.
pub const GAP_TURN: f32 = 16.0;
/// Either side of a tool group, and between two markdown blocks split from one
/// text part. Deliberately identical to [`luma_md::render::MD_BLOCK_GAP`]: the
/// markdown renderer already puts exactly this between its own blocks, so a
/// block that gets split out of a part cannot shift by a pixel when it does.
pub const GAP_TOOL: f32 = luma_md::render::MD_BLOCK_GAP;
/// Everything else within a turn.
pub const GAP_BLOCK: f32 = 8.0;
/// The lane under a settled turn that its timestamp lives in. **Reserved**,
/// always — the stamp only appears on hover, and a lane that appeared with it
/// would move every row below on every pointer cross.
pub const TIMESTAMP_LANE: f32 = 32.0;
/// `ListState` overdraw: how far past the viewport rows are measured.
pub const OVERDRAW_PX: f32 = 320.0;

// -- the bottom pin ----------------------------------------------------------
//
// A reply that arrives while you are reading it must not drag the page, and a
// reply you *are* watching must not run off the bottom. That is one mechanism
// with two states, and these are its numbers.

/// Retains velocity frame to frame — higher glides more.
pub const SPRING_DAMPING: f32 = 0.7;
/// Pull toward the target — higher is snappier.
pub const SPRING_STIFFNESS: f32 = 0.05;
/// Inertia — higher is slower to start and to stop.
pub const SPRING_MASS: f32 = 1.25;
/// The fixed timestep the integration is defined at (60fps). Real elapsed time
/// is expressed in multiples of it, so the motion is frame-rate independent.
pub const SPRING_FRAME_MS: f32 = 1000.0 / 60.0;
/// Cap on simulated sub-frames per tick: after a hitch the spring catches up
/// rather than teleporting.
pub const SPRING_MAX_CATCHUP_FRAMES: f32 = 8.0;
/// EMA rate for the feed-forward growth estimate.
pub const SPRING_GROWTH_EMA: f32 = 0.12;
/// While streaming, chase up to this far above the true bottom — enough to
/// keep the growing tail visible instead of hugging an edge that is moving.
pub const SPRING_CHASE_MAX_LEAD: f32 = 32.0;
/// Within this distance the view counts as exactly pinned.
pub const AT_BOTTOM_PX: f32 = 2.0;
/// Inside this band a scroll *toward* the bottom re-engages the pin.
pub const STICK_THRESHOLD_PX: f32 = 70.0;
/// Farther than this many viewports from the end, teleport and glide the rest —
/// a spring asked to cross a whole history is a long slow ride to nowhere.
pub const GLIDE_MAX_VIEWPORTS: f32 = 2.5;
/// Keep the loop warm this long after landing, so a pause mid-stream resumes at
/// cruise instead of re-accelerating from nothing.
pub const SPRING_SETTLE_GRACE_MS: u64 = 500;
/// Past this distance from the end, offer the jump-to-bottom button.
pub const SCROLL_BUTTON_THRESHOLD_PX: f32 = 320.0;
/// The jump-to-bottom pill.
pub const JUMP_DIAMETER: f32 = 30.0;
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
/// The guide rail down a tool group: where it starts, how wide it is, and how
/// far the chips clear it.
pub const RAIL_INSET: f32 = 12.0;
pub const RAIL_WIDTH: f32 = 1.0;
pub const RAIL_GUTTER: f32 = 11.0;

// -- composer (composer.rs) --------------------------------------------------

/// Vertical padding inside the composer's text box: `pt-4 pb-1`.
pub const TEXTAREA_PAD_V: f32 = 20.0;
/// The text box's grow range, **border-box including its own padding**. The
/// floor applies even when the field is empty — it is what gives the resting
/// composer its height rather than collapsing it to one line.
pub const TEXTAREA_MIN: f32 = 76.0;
pub const TEXTAREA_MAX: f32 = 260.0;
/// One wrapped line in the composer. Re-exported from
/// [`luma_ui::text_input::LINE_HEIGHT`] rather than respelled: the field owns
/// its own line box, and a plate that disagreed with it would grow in
/// fractions of a row.
pub use luma_ui::text_input::LINE_HEIGHT as INPUT_LINE_HEIGHT;
/// The row under the text box that carries the model chip and send / stop.
pub const ACTIONS_ROW_HEIGHT: f32 = 46.0;
/// The pill's hairline, top + bottom.
pub const PILL_BORDER_V: f32 = 2.0;
/// The expanded composer's border-box bounds: the floor when empty, and the
/// ceiling once the content stops growing the plate and starts scrolling
/// inside it.
pub const COMPOSER_MIN_HEIGHT: f32 = TEXTAREA_MIN + ACTIONS_ROW_HEIGHT + PILL_BORDER_V;
pub const COMPOSER_MAX_HEIGHT: f32 = TEXTAREA_MAX + ACTIONS_ROW_HEIGHT + PILL_BORDER_V;
/// The compact pill, border-box: a one-line field with its centering inset,
/// plus the hairline. Shorter than the compact control cluster would need on
/// its own, so the field is what sets this height.
pub const COMPACT_TOTAL_HEIGHT: f32 = 49.0;
/// Below this compact input width the composer always expands: a pill that
/// cannot hold the field and the cluster side by side has no compact layout.
pub const MIN_COMPACT_INPUT_WIDTH: f32 = 200.0;
/// Slack on the expanded→compact flip. Expanding and collapsing share no
/// boundary, so a draft parked at the threshold cannot oscillate.
pub const COLLAPSE_HYSTERESIS: f32 = 32.0;
/// During an interactive resize, collapsing waits until the measured widths
/// have been stable this long. Expansion stays immediate, so a narrowing pane
/// never traps the controls in a compact row.
pub const RESIZE_SETTLE_MS: u64 = 150;
/// How hard the pill blurs the transcript scrolling under it.
pub const PILL_BLUR: f32 = 16.0;
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
