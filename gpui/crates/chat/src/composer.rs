//! What the person types, and the one button that goes with it.
//!
//! # The pill
//!
//! Comet's composer is not a card: it is a *floating pill* — `rounded(26)` over
//! a faint wash, hairline bordered, frosted so the transcript blurs under it
//! rather than stopping at a seam. Everything the composer offers lives inside
//! that one shape. The plate is the one control the eye lands on, which is why
//! it gets a radius of its own rather than the panel's.
//!
//! # Two layouts, one shape
//!
//! Short single-line drafts sit on **one row** (input and controls side by
//! side, 49px tall); anything longer stacks the input over an actions row. The
//! flip between them is [`flip`], and it is hysteretic on purpose: expanding
//! and collapsing share no boundary, so a draft sitting exactly at the
//! threshold cannot oscillate. [`FlipMorph`] then animates the *committed*
//! height — the layout commits immediately (the input entity never remounts, so
//! the caret survives) while the pill's top edge sweeps to the new height.
//!
//! The tween is driven by hand rather than through `with_animation`: that
//! helper keys its timeline by element id and replays on remount, which turns a
//! re-render into a repeated animation. The same reason `luma_ui::pane` drives
//! its resize by hand.
//!
//! # How tall it gets
//!
//! The field grows with its content and then scrolls inside itself. The range
//! is stated in pixels by [`crate::theme`] and honoured by the field, which
//! owns its own max content height — a `max_h` on the wrapper would clip
//! instead of scroll, which is the same shape of bug as measuring a fold.
//!
//! # Why it is never disabled
//!
//! A turn in flight does not lock the field: what the person types during one
//! *steers* it (`TurnStream::steer`), applied at the next row boundary. So the
//! one button reads Send, Steer or Stop — see [`Action`] — and there is no
//! state in which the composer is inert while a conversation is happening.

use std::time::{Duration, Instant};

use gpui::{div, prelude::*, px, Context, Entity, Focusable as _, SharedString, Window};
use gpui_component::{Icon, IconName};
use luma_ui::node::{AgentNode as _, Instrument, Role as NodeRole};
use luma_ui::text_input::{self, TextInput};
use luma_ui::{dialog, motion};

use crate::theme::{self, Theme};
use crate::AgentChat;

/// Placeholder, and the composer's only copy. Public because it is also the
/// composer's *name* in the automation tree — a driver has to find this field
/// among a screen's own inputs, and the label is what tells them apart.
pub const PLACEHOLDER: &str = "Do anything…";

// -- the flip -----------------------------------------------------------------

/// Compact↔expanded flip with hysteresis.
///
/// `capacity` is the **compact-mode** input capacity, and it has to be a
/// layout-stable width: measured directly while compact, tracked by
/// container-width deltas while expanded. Never the post-flip measured width —
/// that differs per mode, so feeding it back in makes the decision depend on
/// its own outcome.
///
/// - a newline always expands: two lines have no compact layout;
/// - during an interactive resize an expanded composer stays expanded, so a
///   narrowing pane never traps the controls in a compact row mid-drag;
/// - a pill too narrow to hold both the field and the cluster always expands;
/// - otherwise compact expands when the text *exceeds* capacity, and expanded
///   collapses only once the text is [`theme::COLLAPSE_HYSTERESIS`] clear of it.
#[must_use]
pub fn flip(
    expanded: bool,
    text_width: f32,
    capacity: f32,
    has_newline: bool,
    resizing: bool,
) -> bool {
    if has_newline || capacity < theme::MIN_COMPACT_INPUT_WIDTH {
        return true;
    }
    if expanded {
        resizing || text_width >= capacity - theme::COLLAPSE_HYSTERESIS
    } else {
        text_width > capacity
    }
}

/// Total expanded height, border-box, for a shaped content height: the text
/// box (content + its padding) clamps to the grow range, then the actions row
/// and the hairline ride on top.
#[must_use]
pub fn total_height(content_height: f32) -> f32 {
    (content_height + theme::TEXTAREA_PAD_V).clamp(theme::TEXTAREA_MIN, theme::TEXTAREA_MAX)
        + theme::ACTIONS_ROW_HEIGHT
        + theme::PILL_BORDER_V
}

/// One flip's height tween.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlipMorph {
    /// Rendered height when the flip committed — the tween's start point.
    pub from: f32,
    /// Commit time in ms on the composer's own monotonic clock.
    pub start_ms: f32,
}

impl FlipMorph {
    fn raw(self, now_ms: f32) -> f32 {
        // The measurement knob is folded in here, through the one shared
        // `motion::span`, so `now_ms` is plain wall-clock milliseconds.
        let total = motion::span(&motion::COLLAPSE).as_secs_f32() * 1000.0;
        ((now_ms - self.start_ms) / total).clamp(0.0, 1.0)
    }

    /// Eased progress 0..1 — also drives the text and cluster glides.
    #[must_use]
    pub fn progress(self, now_ms: f32) -> f32 {
        motion::COLLAPSE.progress(self.raw(now_ms))
    }

    #[must_use]
    pub fn done(self, now_ms: f32) -> bool {
        self.raw(now_ms) >= 1.0
    }

    /// Eased lerp from the flip-time height to the **live** target: auto-grow
    /// can move the target mid-tween, and the morph tracks it rather than
    /// finishing on a height that is already stale.
    #[must_use]
    pub fn height(self, target: f32, now_ms: f32) -> f32 {
        motion::lerp(self.from, target, self.progress(now_ms))
    }
}

/// Advance the morph across one render pass.
///
/// While the committed mode holds, a running morph is kept and a finished one
/// clears — a same-mode render can never restart the animation. A mode change
/// starts one morph from the last *rendered* height, which mid-flight is the
/// current animated height, so a reverse flip hands off seamlessly instead of
/// popping to an endpoint. Reduced motion, and a first paint with nothing
/// measured yet, both snap.
#[must_use]
pub fn morph_step(
    morph: Option<FlipMorph>,
    mode_changed: bool,
    last_height: f32,
    now_ms: f32,
    reduced_motion: bool,
) -> Option<FlipMorph> {
    if !mode_changed {
        return morph.filter(|m| !m.done(now_ms));
    }
    if reduced_motion || last_height <= 0.0 {
        return None;
    }
    Some(FlipMorph {
        from: last_height,
        start_ms: now_ms,
    })
}

// -- morph anchoring ----------------------------------------------------------
//
// The pill sits at the BOTTOM of its column, so growing it moves its TOP edge
// and the bottom edge is stationary on screen. The controls are therefore
// pinned to that stationary edge and only the TEXT glides with the sweeping
// top. Anchoring the inner content to the top instead makes the cluster ride
// the animating height up and down, which reads as the buttons bouncing.

/// The send cluster sits 27px above the pill's outer bottom when expanded but
/// 24.5px when compact — an inherent difference between the two geometries.
/// The morph glides it rather than letting it snap.
pub const CLUSTER_Y_DELTA: f32 = 2.5;
/// The cluster's right inset differs by mode (8 compact, 12 expanded). Only the
/// wrapper moves: pairwise button distances are mode-independent, so the flip
/// cannot create a horizontal compression pulse.
pub const CLUSTER_X_DELTA: f32 = 4.0;

/// The cluster's right inset mid-morph: eases from the old mode's resting
/// inset to the committed one.
#[must_use]
pub fn cluster_inset(expanded: bool, progress: f32) -> f32 {
    let (from, to) = if expanded {
        (8.0, 8.0 + CLUSTER_X_DELTA)
    } else {
        (8.0 + CLUSTER_X_DELTA, 8.0)
    };
    motion::lerp(from, to, progress)
}

/// The expanded text's top padding across the morph: starts at the compact
/// resting inset (12) and eases to 16, so the first line glides with the rising
/// top edge instead of jumping at the commit.
#[must_use]
pub fn text_pad(progress: f32) -> f32 {
    motion::lerp(12.0, 16.0, progress)
}

/// Collapse-morph text glide. The committed compact row is bottom-anchored,
/// with its text resting 36px above the pill's outer bottom; at the commit
/// instant the text sat 17px below the expanded pill's top, i.e. `from − 17`
/// above the bottom. The decaying relative offset walks it down smoothly.
#[must_use]
pub fn collapse_text_glide(from: f32, progress: f32) -> f32 {
    (from - 53.0).max(0.0) * (1.0 - progress)
}

/// The decaying [`CLUSTER_Y_DELTA`] offset. The cluster rides at full alpha
/// throughout: its screen position is near-stationary across the flip, so
/// nothing needs hiding, and a fade on it reads as a flicker.
#[must_use]
pub fn cluster_dy(progress: f32) -> f32 {
    CLUSTER_Y_DELTA * (1.0 - progress)
}

// -- the button ---------------------------------------------------------------

/// What the one button does, which is a function of two facts and nothing
/// else: whether a turn is running, and whether anything is typed.
///
/// One button in three tenses rather than three buttons, because a turn is
/// either running or it is not — a pair that could disagree about that is a
/// state the panel must not be able to reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing to send and no turn to stop. The only inert state the button
    /// has — notably *not* "a turn is running".
    Inert,
    /// No turn running, and something to say: start one.
    Send,
    /// A turn is running and there is text: redirect it at the next row
    /// boundary rather than queueing a second turn behind it.
    Steer,
    /// A turn is running and there is nothing to say: end it.
    Stop,
}

impl Action {
    /// The button's whole state, from the only two facts it depends on.
    ///
    /// Every combination names one action, including the inert one — so the
    /// button carries no second "enabled" flag that could contradict it, and
    /// there is no "disabled while streaming" case left to get wrong.
    #[must_use]
    pub fn of(streaming: bool, has_text: bool) -> Self {
        match (streaming, has_text) {
            (false, false) => Action::Inert,
            (false, true) => Action::Send,
            (true, true) => Action::Steer,
            (true, false) => Action::Stop,
        }
    }

    /// What the automation tree calls it. [`Action::Inert`] is still the send
    /// button — it reports so, and reports itself disabled.
    fn label(self) -> &'static str {
        match self {
            Action::Inert | Action::Send => "Send",
            Action::Steer => "Steer",
            Action::Stop => "Stop",
        }
    }
}

// -- the composer -------------------------------------------------------------

/// The field, its plate, and the flip state that decides which shape the plate
/// is in.
///
/// A plain struct owned by [`AgentChat`] rather than an entity of its own: all
/// of this is *one* control's private layout state, and a second entity would
/// buy an event hop between two things that already re-render together. The
/// field inside it is an entity, because a text editor genuinely owns state.
pub struct Composer {
    input: Entity<TextInput>,
    /// Which layout is committed. Not derived per frame: the flip is
    /// hysteretic, so it is a function of its own previous value.
    expanded: bool,
    /// The layout epoch the last flip consumed. A flip invalidates the widths
    /// it was decided on, so at most one flip lands per measurement.
    flip_epoch: u64,
    /// Compact-mode input capacity, learned while compact and carried while
    /// expanded — see [`flip`] on why the live expanded width will not do.
    compact_capacity: f32,
    /// The expanded width at which [`Self::compact_capacity`] was last valid,
    /// so a container resize while expanded shifts the carried capacity by the
    /// same amount.
    expanded_anchor: f32,
    last_seen_width: f32,
    /// When the container width last changed — an interactive resize defers
    /// collapse until it settles.
    width_changed_at: Option<Instant>,
    /// Wakes the composer once the settle window has passed, so a resize that
    /// stops moving still gets its collapse.
    settle: Option<gpui::Task<()>>,
    morph: Option<FlipMorph>,
    /// Monotonic origin for the morph timeline, in plain milliseconds — the
    /// measurement knob is folded into [`FlipMorph::raw`]'s span, not here.
    clock: Instant,
    /// What the pill actually rendered at last frame — the start height a
    /// reverse flip hands off from.
    last_height: f32,
}

impl Composer {
    /// A fresh composer, subscribed so its host hears submits and escapes.
    pub fn new(chat: &mut Context<AgentChat>) -> Self {
        let input = chat.new(|cx| {
            let mut input =
                TextInput::composer(PLACEHOLDER, theme::TEXTAREA_MAX - theme::TEXTAREA_PAD_V, cx);
            input.set_style(input_style(&Theme::dark()), cx);
            // The composer holds focus while the reader reads the thread, so a
            // bare Copy there is about the transcript, not this empty field.
            input.set_copy_fallback(luma_md::selection::selected_text);
            input
        });
        chat.subscribe(&input, |chat, _, event, cx| match event {
            // Every edit can change the button's tense and the pill's height,
            // so the panel re-renders on all of them; the field itself has
            // already notified, this is the host catching up.
            text_input::Event::Edited
            | text_input::Event::CursorMoved
            | text_input::Event::ViewportChanged => cx.notify(),
            text_input::Event::Submitted => chat.send(cx),
            text_input::Event::Cancelled => chat.escape(cx),
        })
        .detach();
        Self {
            input,
            expanded: false,
            flip_epoch: 0,
            compact_capacity: 0.0,
            expanded_anchor: 0.0,
            last_seen_width: 0.0,
            width_changed_at: None,
            settle: None,
            morph: None,
            clock: Instant::now(),
            last_height: 0.0,
        }
    }

    /// What is typed, trimmed — what a send would actually carry.
    #[must_use]
    pub fn prompt(&self, cx: &gpui::App) -> String {
        self.input.read(cx).text().trim().to_string()
    }

    /// Empty the field. A send does this, and so does a steer.
    pub fn clear(&self, cx: &mut gpui::App) {
        self.input.update(cx, |input, cx| input.set_text("", cx));
    }

    /// Fill the field and put the caret in it — what the empty state's prompts
    /// do. They *offer* a question; they do not ask it.
    pub fn suggest(&self, prompt: &str, window: &mut Window, cx: &mut gpui::App) {
        self.input.update(cx, |input, cx| {
            input.set_text(prompt, cx);
            window.focus(&input.focus_handle(cx), cx);
        });
    }

    /// Where window-level focus lands.
    #[must_use]
    pub fn focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }

    /// Decide this frame's layout from the field's last measurements, and
    /// arm or advance the morph. Split out from painting because it is the only
    /// part with a decision in it — everything below is geometry.
    fn settle_layout(&mut self, reduced_motion: bool, cx: &mut Context<AgentChat>) -> Layout {
        let (text_width, has_newline, content_height, width, epoch) = {
            let input = self.input.read(cx);
            (
                input.measured_text_width(),
                input.has_newline(),
                input.measured_content_height(),
                input.measured_width(),
                input.layout_epoch(),
            )
        };
        let now = Instant::now();
        // Only measurements taken *after* the last flip may drive the next one.
        let measured = epoch > self.flip_epoch && width > 0.0;
        if measured {
            // A same-mode width change is an interactive resize of the pane.
            if self.last_seen_width > 0.0 && (width - self.last_seen_width).abs() > 0.5 {
                self.width_changed_at = Some(now);
            }
            self.last_seen_width = width;
            if self.expanded {
                if self.expanded_anchor <= 0.0 {
                    self.expanded_anchor = width;
                }
            } else {
                // The compact pill's content box is the layout-stable capacity
                // both thresholds are measured against.
                self.compact_capacity = width - 8.0;
            }
        }
        let settle = Duration::from_millis(theme::RESIZE_SETTLE_MS);
        let resizing = self
            .width_changed_at
            .is_some_and(|at| now.duration_since(at) < settle);
        if resizing && self.settle.is_none() {
            self.settle = Some(cx.spawn(async move |chat, cx| {
                cx.background_executor()
                    .timer(settle + Duration::from_millis(20))
                    .await;
                chat.update(cx, |chat, cx| {
                    chat.composer.settle = None;
                    cx.notify();
                })
                .ok();
            }));
        }
        let capacity = if !self.expanded {
            // Before the first measurement, default to compact: an empty
            // composer has nothing to overflow.
            if width > 0.0 {
                width - 8.0
            } else {
                f32::MAX
            }
        } else if self.compact_capacity > 0.0 {
            if self.expanded_anchor > 0.0 && width > 0.0 {
                self.compact_capacity + (width - self.expanded_anchor)
            } else {
                self.compact_capacity
            }
        } else {
            f32::MAX
        };

        let next = flip(self.expanded, text_width, capacity, has_newline, resizing);
        let committed = next != self.expanded && measured;
        if committed {
            self.expanded = next;
            self.flip_epoch = epoch;
            self.expanded_anchor = 0.0;
            // The mode change moves the input width; that jump is not a resize.
            self.last_seen_width = 0.0;
        }
        let now_ms = self.clock.elapsed().as_secs_f32() * 1000.0;
        self.morph = morph_step(
            self.morph,
            committed,
            self.last_height,
            now_ms,
            reduced_motion,
        );

        let base = if self.expanded {
            total_height(content_height)
        } else {
            theme::COMPACT_TOTAL_HEIGHT
        };
        let (height, progress, morphing) = match self.morph {
            Some(m) if !m.done(now_ms) => (m.height(base, now_ms), m.progress(now_ms), true),
            _ => (base, 1.0, false),
        };
        if !morphing {
            self.morph = None;
        }
        self.last_height = height;
        Layout {
            expanded: self.expanded,
            base,
            height,
            progress,
            morphing,
            glide: match self.morph {
                Some(m) if morphing => collapse_text_glide(m.from, progress),
                _ => 0.0,
            },
        }
    }
}

/// One frame's resolved pill geometry — the output of [`Composer::settle_layout`]
/// and the only thing the painting below reads.
struct Layout {
    expanded: bool,
    /// The committed layout's own height, which the text box is sized from.
    /// The morph animates the *outer* height past it; sizing the inner content
    /// from the animated value would reflow the text mid-tween and jump the
    /// caret.
    base: f32,
    height: f32,
    progress: f32,
    morphing: bool,
    glide: f32,
}

/// The field's four colours, from the chat's palette.
fn input_style(theme: &Theme) -> text_input::Style {
    text_input::Style {
        text: theme.text,
        placeholder: theme.text_faint,
        selection: theme.accent.opacity(0.28),
        caret: theme.text,
    }
}

/// The composer plate: the field, and send or stop.
///
/// Takes the composer by `&mut` rather than reaching for it through `chat`:
/// this runs *inside* the panel's own render, so re-entering the entity to
/// reach a field of the struct that is already borrowed would panic.
pub fn composer(
    composer: &mut Composer,
    chat: &Entity<AgentChat>,
    streaming: bool,
    model: Option<&str>,
    theme: &Theme,
    window: &mut Window,
    cx: &mut Context<AgentChat>,
) -> impl IntoElement {
    let reduced = motion::reduced_motion(cx);
    let layout = composer.settle_layout(reduced, cx);
    if layout.morphing {
        // Manual tween drive: keep frames coming while the height sweeps.
        window.request_animation_frame();
    }
    let input = composer.input.clone();
    let value = input.read(cx).text().to_string();
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    let action = Action::of(streaming, !value.trim().is_empty());
    let dy = cluster_dy(layout.progress);

    // The pill: a floating shape over a faint wash with a hairline, never a
    // solid grey box. The shadow is gated off wherever the backdrop actually
    // blurs, because on glass a drop shadow paints *behind* the translucent
    // fill and shows through as an inner glow. Where there is no blur there is
    // no glass to show through, and the pill needs the lift.
    let pill = div()
        .h(px(layout.height))
        .overflow_hidden()
        .rounded(px(luma_ui::radius::PILL))
        .bg(theme.input_bg)
        .border_1()
        .border_color(theme.border)
        .when(!dialog::BACKDROP_BLUR_SUPPORTED, gpui::Div::shadow_lg);

    let field = div()
        .flex_1()
        .min_w_0()
        .child(input.clone())
        .agent_node(
            NodeRole::Input,
            if value.is_empty() {
                SharedString::from(PLACEHOLDER)
            } else {
                SharedString::from(value)
            },
        )
        .agent_focused(focused);

    let cluster = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_SM))
        .children(model.map(|model| model_chip(model, theme)))
        .child(send(chat, action, theme));

    let body = if layout.expanded {
        // Expanded: the field on top, the actions row pinned ABSOLUTE at the
        // pill's stationary bottom — constant screen-y through the morph, with
        // the centering delta gliding out. The text box is laid out at the
        // TARGET size so the committed layout never reflows mid-tween.
        pill.relative()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px((layout.base
                        - theme::PILL_BORDER_V
                        - theme::ACTIONS_ROW_HEIGHT)
                        .max(0.0)))
                    .px(px(theme::SPACE_LG))
                    .pt(px(text_pad(layout.progress)))
                    .pb(px(theme::SPACE_XS))
                    .child(field),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(-dy))
                    .h(px(theme::ACTIONS_ROW_HEIGHT))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .pl(px(theme::SPACE_MD))
                    .pr(px(cluster_inset(true, layout.progress)))
                    .pt(px(theme::SPACE_XS))
                    .pb(px(10.0))
                    .child(cluster),
            )
    } else {
        // Compact: field and cluster on one row, BOTTOM-justified. During a
        // collapse the pill's top sweeps down over a stationary row while the
        // text walks down from its expanded resting place.
        pill.flex().flex_col().justify_end().child(
            div()
                .h(px(theme::COMPACT_TOTAL_HEIGHT - theme::PILL_BORDER_V))
                .flex()
                .flex_row()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl(px(theme::SPACE_LG))
                        .pr(px(theme::SPACE_SM))
                        .relative()
                        .top(px(-layout.glide))
                        .child(field),
                )
                .child(
                    div()
                        .flex_none()
                        .pl(px(theme::SPACE_XS))
                        .pr(px(cluster_inset(false, layout.progress)))
                        .relative()
                        .top(px(-dy))
                        .child(cluster),
                ),
        )
    };

    // The plate sits on the same reading column as the transcript above it —
    // a full-bleed composer under a centered column is two column rules on
    // one pane. Frosted so the transcript blurs under it instead of stopping
    // at a seam; the radius matches the pill's, or the blur would square off
    // at its corners.
    div()
        .flex()
        .flex_col()
        .items_center()
        .px(px(theme::CONTENT_GUTTER))
        .py(px(theme::SPACE_MD))
        .child(
            div()
                .w_full()
                .max_w(px(theme::MAX_CONTENT_WIDTH))
                .child(dialog::frosted(
                    luma_ui::radius::PILL,
                    theme::PILL_BLUR,
                    motion::fade_quick("composer-pill", body),
                )),
        )
}

/// Which model answers. A readout, not a control — the model is chosen in
/// settings, and a second place to change it would be a second rule about it.
fn model_chip(model: &str, theme: &Theme) -> impl IntoElement {
    let label = SharedString::from(model.to_string());
    div()
        .h(px(theme::CHIP_SMALL_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_XS + 2.0))
        .px(px(theme::SPACE_SM))
        .rounded(px(luma_ui::radius::CONTROL))
        .bg(theme::card_bg())
        .border_1()
        .border_color(theme.border)
        .child(
            Icon::new(IconName::Bot)
                .size(px(12.0))
                .text_color(theme.text_faint),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_muted)
                .child(label.clone()),
        )
        .agent_node(NodeRole::Text, label)
}

/// Send, steer, or stop.
///
/// One circular slug in every tense — an arrow to send or steer, a square to
/// stop — because the eye tracks a shape that stays put and swaps its glyph,
/// and a word that changes width does not.
fn send(chat: &Entity<AgentChat>, action: Action, theme: &Theme) -> impl IntoElement {
    let pressed = chat.clone();
    let enabled = action != Action::Inert;
    div()
        .id("chat-send")
        .size(px(theme::SEND_DIAMETER))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        // A filled light disc in every state, dimmed only in its *fill*.
        // Fading the whole slug composited the dark glyph away too, and on
        // these near-black grounds the result was a grey disc on grey — the
        // primary action reading as the least visible thing on the panel.
        .bg(if enabled {
            theme.text
        } else {
            theme.text.opacity(0.72)
        })
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(|style| style.opacity(0.85))
                .on_click(move |_, _, cx| {
                    pressed.update(cx, |this, cx| match action {
                        Action::Stop => this.cancel(cx),
                        Action::Send | Action::Steer => this.send(cx),
                        // Unreachable: an inert button has no click handler.
                        Action::Inert => {}
                    });
                })
        })
        .child(if action == Action::Stop {
            div()
                .size(px(11.0))
                .rounded(px(luma_ui::radius::CHIP))
                .bg(theme.knockout)
                .into_any_element()
        } else {
            Icon::new(IconName::ArrowUp)
                .size(px(14.0))
                .text_color(theme.knockout)
                .into_any_element()
        })
        .agent_node(NodeRole::Button, action.label())
        .agent_disabled(!enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The button's four states, and the one that is inert. A turn running is
    /// never inert — that was the bug: the composer went dead mid-turn.
    #[test]
    fn only_an_idle_empty_composer_is_inert() {
        assert_eq!(Action::of(false, false), Action::Inert);
        assert_eq!(Action::of(false, true), Action::Send);
        assert_eq!(Action::of(true, true), Action::Steer);
        assert_eq!(Action::of(true, false), Action::Stop);
    }

    /// The whole point of the hysteresis: the width that expands and the width
    /// that collapses are different, so a draft parked at the boundary settles
    /// on one layout instead of flickering between two.
    #[test]
    fn expanding_and_collapsing_share_no_boundary() {
        let capacity = 400.0;
        assert!(!flip(false, capacity, capacity, false, false));
        assert!(flip(true, capacity, capacity, false, false));
        // Just inside the collapse threshold: still expanded.
        let inside = capacity - theme::COLLAPSE_HYSTERESIS + 1.0;
        assert!(flip(true, inside, capacity, false, false));
        assert!(!flip(false, inside, capacity, false, false));
        // Clear of it: collapses.
        assert!(!flip(
            true,
            capacity - theme::COLLAPSE_HYSTERESIS - 1.0,
            capacity,
            false,
            false
        ));
    }

    /// Three facts override the width entirely: a newline has no compact
    /// layout, a pill too narrow to hold both halves has none either, and an
    /// in-flight resize must not collapse under the pointer.
    #[test]
    fn a_newline_a_narrow_pill_and_a_live_resize_all_force_expanded() {
        assert!(flip(false, 0.0, 400.0, true, false));
        assert!(flip(
            false,
            0.0,
            theme::MIN_COMPACT_INPUT_WIDTH - 1.0,
            false,
            false
        ));
        assert!(flip(true, 0.0, 400.0, false, true));
        // …but a resize does not *expand* a compact composer: the controls
        // never squeeze the field away mid-drag.
        assert!(!flip(false, 0.0, 400.0, false, true));
    }

    /// The grow range is a range, and the plate can hold both ends of it.
    #[test]
    fn the_pill_height_spans_the_declared_range() {
        assert!(theme::TEXTAREA_MIN < theme::TEXTAREA_MAX);
        assert_eq!(total_height(0.0), theme::COMPOSER_MIN_HEIGHT);
        assert_eq!(total_height(f32::MAX), theme::COMPOSER_MAX_HEIGHT);
        // One empty line still renders the floor, not something shorter.
        assert_eq!(
            total_height(text_input::LINE_HEIGHT),
            theme::COMPOSER_MIN_HEIGHT
        );
    }

    /// A same-mode render can never restart the tween, and a reverse flip
    /// hands off from the height on screen rather than popping to an endpoint.
    #[test]
    fn a_morph_starts_only_on_a_committed_flip_and_never_restarts() {
        let running = FlipMorph {
            from: 49.0,
            start_ms: 0.0,
        };
        assert_eq!(
            morph_step(Some(running), false, 124.0, 50.0, false),
            Some(running)
        );
        assert_eq!(
            morph_step(Some(running), false, 124.0, 9_999.0, false),
            None
        );
        let flipped = morph_step(Some(running), true, 90.0, 50.0, false).expect("armed");
        assert_eq!(flipped.from, 90.0, "hands off from what is on screen");
        assert_eq!(
            morph_step(None, true, 124.0, 50.0, true),
            None,
            "reduced motion snaps"
        );
        assert_eq!(
            morph_step(None, true, 0.0, 50.0, false),
            None,
            "nothing measured yet"
        );
    }

    /// Both glides decay to nothing, so the steady state is exactly the
    /// committed layout and no morph leaves a permanent offset behind.
    #[test]
    fn every_glide_lands_on_the_committed_geometry() {
        assert_eq!(cluster_dy(1.0), 0.0);
        assert_eq!(collapse_text_glide(308.0, 1.0), 0.0);
        assert_eq!(text_pad(1.0), 16.0);
        assert_eq!(cluster_inset(true, 1.0), 8.0 + CLUSTER_X_DELTA);
        assert_eq!(cluster_inset(false, 1.0), 8.0);
    }
}
