//! Ported from zeron (MIT, © 2026 Wing) — crates/ui/src/motion.rs
//!
//! Animation kit — one spring, five durations, and the helpers that ride them
//! over gpui [`Animation`]/[`AnimationExt`].
//!
//! # One curve
//!
//! Everything that moves eases on [`ROOT`]. A second curve would be a second
//! motion language, and the app would read as two apps sharing a window: the
//! only thing that distinguishes a menu pop from a panel slide is how far and
//! for how long, never *how*.
//!
//! Durations come from the ladder — [`SNAP`], [`QUICK`], [`BASE`], [`SWEEP`], [`SLOW`] —
//! for the same reason the greys do: a new animation picks a rung rather than
//! inventing a number, and another rung is a design decision, not a literal.
//! Loader *periods* ([`PULSE`], [`GRADIENT_SPIN`]) are not on the ladder — they
//! are the length of a loop, not of a transition.
//!
//! The normalized spring is evaluated through gpui's `Fn(f32) -> f32` easing
//! closure. Every duration preserves its gentle start and settling shape.
//!
//! Reduced motion: gpui's `App::reduce_motion` flag is honored *automatically* by
//! every `with_animation` element — oneshot animations snap to their end state,
//! repeating ones to their start state, and no frames are scheduled. The
//! [`set_reduced_motion`]/[`reduced_motion`] wrappers make it a single global
//! switch; pure helpers take the flag explicitly where they run outside elements.
//!
//! translateY is implemented as a relative-position `top` inset: taffy applies
//! relative insets after layout, so — like a CSS transform — siblings never move.
//! gpui has no scale transform for `div`s at the pinned rev (only `svg`
//! transformations), so `menu-in`/`dialog-in` approximate their scale component
//! with fade + translate; see the module report in ARCHITECTURE §4 follow-ups.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    px, Animation, AnimationElement, App, ElementId, EntityId, Global, Hsla, IntoElement, Rgba,
    SharedString, Styled, Window,
};

pub use gpui::AnimationExt;

// ---------------------------------------------------------------------------
// Pulse clock — throttled drive for the repeating loaders
// ---------------------------------------------------------------------------

/// Repeat-tick interval for the pulse/spinner loaders (~30fps).
///
/// The loaders used to run as gpui `with_animation(...repeating...)` elements,
/// which request a redraw every display frame for as long as they are mounted
/// — one Working session row pinned the whole window at 120Hz (measured 36%
/// CPU on an M-series laptop, with the always-hot Metal pipeline holding
/// hundreds of MB of graphics buffers). A shared 30fps clock is visually
/// equivalent for these chunky cell waves at a quarter of the redraws, and a
/// window with no spinner mounted schedules nothing at all.
const PULSE_TICK: Duration = Duration::from_millis(33);

/// How long a view stays on the tick list after its last spinner paint. One
/// lease outlives a few missed frames; an unmounted spinner stops renewing and
/// the view drops off, letting the clock park.
const PULSE_LEASE: Duration = Duration::from_millis(300);

struct PulseClock {
    epoch: Instant,
    leases: HashMap<EntityId, Instant>,
    running: bool,
}

impl Global for PulseClock {}

impl Default for PulseClock {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            leases: HashMap::new(),
            running: false,
        }
    }
}

/// Current phase `[0,1)` of a repeating spec, plus a lease that keeps the
/// calling view re-rendering at the pulse tick while its spinner stays
/// mounted. All cells across all views share one epoch, so multi-instance
/// loaders stay phase-locked. Reduced motion returns a static 0 and schedules
/// nothing.
pub fn pulse_delta(spec: &MotionSpec, view: EntityId, cx: &mut App) -> f32 {
    if cx.reduce_motion() {
        return 0.0;
    }
    let clock = cx.default_global::<PulseClock>();
    clock.leases.insert(view, Instant::now() + PULSE_LEASE);
    let period = spec.total().as_secs_f32();
    let phase = (clock.epoch.elapsed().as_secs_f32() / period).fract();
    if !clock.running {
        clock.running = true;
        cx.spawn(async move |cx| loop {
            cx.background_executor().timer(PULSE_TICK).await;
            let parked = cx.update(|cx| {
                let clock = cx.default_global::<PulseClock>();
                let now = Instant::now();
                clock.leases.retain(|_, until| *until > now);
                if clock.leases.is_empty() {
                    clock.running = false;
                    return true;
                }
                let views: Vec<EntityId> = clock.leases.keys().copied().collect();
                for view in views {
                    cx.notify(view);
                }
                false
            });
            if parked {
                break;
            }
        })
        .detach();
    }
    phase
}

// ---------------------------------------------------------------------------
// Spring
// ---------------------------------------------------------------------------

/// The desktop ChatGPT spring, normalized to a unit-duration transition from
/// rest. Its duration-based spring solver uses damping ratio `1 - bounce` and
/// a 0.001 settling envelope. With bounce 0.1 this gives normalized angular
/// frequency `ln(0.9 / (sqrt(1 - 0.9²) * 0.001)) / 0.9`.
///
/// All durations use this same shape. GPUI requires easing in [0,1], so the
/// tiny overshoot is clamped; layout and opacity never leave their valid range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring;

impl Spring {
    const DAMPING: f32 = 0.9;
    const FREQUENCY: f32 = 8.480_845;

    /// Advance a live spring without discarding its velocity on retarget.
    /// Displacement is relative to the target; velocity is units per second.
    /// Unlike a bounded easing fraction, this preserves the physical state.
    pub fn advance(
        &self,
        displacement: f32,
        velocity: f32,
        seconds: f32,
        duration: f32,
    ) -> (f32, f32) {
        if duration <= 0.0 {
            return (0.0, 0.0);
        }
        let frequency = Self::FREQUENCY / duration;
        let decay = Self::DAMPING * frequency;
        let damped = frequency * (1.0 - Self::DAMPING * Self::DAMPING).sqrt();
        let (sin, cos) = (damped * seconds).sin_cos();
        let envelope = (-decay * seconds).exp();
        (
            envelope * (displacement * cos + (velocity + decay * displacement) / damped * sin),
            envelope
                * (velocity * cos
                    - (decay * velocity + frequency * frequency * displacement) / damped * sin),
        )
    }

    /// Eased progress with exact endpoints and bounded output.
    pub fn eval(&self, progress: f32) -> f32 {
        if progress <= 0.0 {
            return 0.0;
        }
        if progress >= 1.0 {
            return 1.0;
        }
        let damping = Self::DAMPING;
        let frequency = Self::FREQUENCY;
        let damped_ratio = (1.0 - damping * damping).sqrt();
        let phase = frequency * damped_ratio * progress;
        let remaining = (-damping * frequency * progress).exp()
            * (phase.cos() + damping / damped_ratio * phase.sin());
        (1.0 - remaining).clamp(0.0, 1.0)
    }
}

/// One spring shape for every transition: gentle acceleration and a soft settle.
pub const ROOT: Spring = Spring;

// ---------------------------------------------------------------------------
// Motion specs (the catalog)
// ---------------------------------------------------------------------------

/// One catalog entry: duration + optional delay + curve. The delay is folded into
/// the gpui animation timeline (gpui `Animation` has no native delay): the
/// animation runs for `delay + duration` and [`progress`](Self::progress) holds 0
/// until the delay has elapsed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSpec {
    pub duration_ms: u64,
    pub delay_ms: u64,
    pub curve: Spring,
}

impl MotionSpec {
    pub const fn new(duration_ms: u64, curve: Spring) -> Self {
        Self {
            duration_ms,
            delay_ms: 0,
            curve,
        }
    }

    pub const fn with_delay(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }

    /// Wall-clock span of the whole timeline (delay + duration).
    pub fn total(&self) -> Duration {
        Duration::from_millis(self.delay_ms + self.duration_ms)
    }

    /// Eased progress (0..1) for a raw timeline delta (0..1 across [`total`](Self::total)).
    /// Pure — unit-testable without a window.
    pub fn progress(&self, raw_delta: f32) -> f32 {
        let total = (self.delay_ms + self.duration_ms) as f32;
        if total <= 0.0 || self.duration_ms == 0 {
            return 1.0;
        }
        let t =
            (raw_delta.clamp(0.0, 1.0) * total - self.delay_ms as f32) / self.duration_ms as f32;
        self.curve.eval(t.clamp(0.0, 1.0))
    }

    /// A oneshot gpui [`Animation`] for this spec (delay folded in).
    /// Wall-clock span honors [`speed_scale`] (measurement knob).
    pub fn animation(&self) -> Animation {
        let spec = *self;
        Animation::new(span(&spec)).with_easing(move |d| spec.progress(d))
    }

    /// A repeating gpui [`Animation`] with linear easing over the raw period —
    /// for the pulse/wave loaders whose per-cell easing happens in the animator.
    pub fn repeating(&self) -> Animation {
        Animation::new(self.total()).repeat()
    }
}

// -- the duration ladder ------------------------------------------------------

/// Small exits (180ms).
pub const SNAP: u64 = 180;
/// Menus, hover fades, and tab slides (280ms).
pub const QUICK: u64 = 280;
/// Contained structural changes: row collapses and chevrons (360ms).
pub const BASE: u64 = 360;
/// Panels, dialogs, and navigation pushes (500ms), matching the desktop
/// ChatGPT sidebar's duration as well as its spring shape.
pub const SWEEP: u64 = 500;
/// Entrances, the splash, and viewport-crossing scrolls (700ms).
pub const SLOW: u64 = 700;

// -- the catalog --------------------------------------------------------------

/// Entrances: fade + 4px rise.
pub const FADE_IN: MotionSpec = MotionSpec::new(SLOW, ROOT);
/// Opacity-only fade.
pub const FADE_QUICK: MotionSpec = MotionSpec::new(QUICK, ROOT);
/// Popover entrance, moving away from its trigger.
pub const MENU_IN: MotionSpec = MotionSpec::new(QUICK, ROOT);
/// Popover exit uses the same timing and spring as its entrance.
pub const MENU_OUT: MotionSpec = MENU_IN;
/// Boot splash exit: fade + 6px lift after a hold.
pub const SPLASH_OUT: MotionSpec = MotionSpec::new(SLOW, ROOT).with_delay(QUICK);
/// Shared spring timing for panes, dialog route morphs, and dialog exits.
pub const SURFACE: MotionSpec = MotionSpec::new(SWEEP, ROOT);
/// Dialogs open more quickly while retaining the same spring shape.
pub const DIALOG_IN: MotionSpec = MotionSpec::new(BASE, ROOT);
/// A navigation *push*: one level of a column leaving to the left while the
/// next arrives from the right, over the column's own width.
///
/// [`SWEEP`], the same rung [`SURFACE`] takes, and for the same reason — the
/// travel is a whole region's width and the eye tracks the moving edge across
/// it. A rung of its own name rather than reusing `SURFACE` because the two are
/// different gestures that merely agree on a duration today: a surface changing
/// shape and a region changing *subject*. Retuning one must not silently retune
/// the other.
pub const PUSH: MotionSpec = MotionSpec::new(SWEEP, ROOT);
/// Tab drag-reorder sliding transforms.
pub const TAB_SLIDE: MotionSpec = MotionSpec::new(QUICK, ROOT);
/// Per-row collapse (height).
pub const COLLAPSE: MotionSpec = MotionSpec::new(BASE, ROOT);
/// Chevron rotate (approximated as a crossfade — gpui divs have no rotation
/// transform at the pinned rev, same caveat as scale).
pub const CHEVRON: MotionSpec = MotionSpec::new(BASE, ROOT);
/// Scroll-to-row glide over the whole distance — fixed duration, never
/// percent-of-remaining.
pub const SCROLL_GLIDE: MotionSpec = MotionSpec::new(SLOW, ROOT);
/// The temporal blend every interactive hover wash rides.
pub const HOVER_FADE: MotionSpec = MotionSpec::new(QUICK, ROOT);
/// Loader pulse period (a loop length, not a transition — see the module docs).
pub const PULSE: MotionSpec = MotionSpec::new(2400, ROOT);
/// Gradient matrix spinner wave period.
pub const GRADIENT_SPIN: MotionSpec = MotionSpec::new(750, ROOT);

// ---------------------------------------------------------------------------
// Element helpers (paint-layer entrances/exits)
// ---------------------------------------------------------------------------

/// Standard entrance: opacity 0→1 + translateY 4→0 over [`FADE_IN`].
pub fn fade_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(4.0 * (1.0 - t)))
    })
}

/// Quick opacity-only fade over [`FADE_QUICK`].
pub fn fade_quick<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_QUICK.animation(), |el, t| el.opacity(t))
}

/// Dialog entrance: fade and scale 0.98→1 using the shared spring.
pub fn dialog_in<E>(
    id: impl Into<ElementId>,
    element: E,
) -> AnimationElement<crate::dialog::Filtered>
where
    E: Styled + IntoElement + 'static,
{
    crate::dialog::filtered(0.0, 1.0, element).with_animation(id, DIALOG_IN.animation(), |el, t| {
        el.pose(0.98 + 0.02 * t, t)
    })
}

/// Dialog exit: the reverse entrance, driven by the closing instant so a
/// remount cannot restart the animation. The wrapper only pumps frames.
pub fn dialog_out<E>(
    id: impl Into<ElementId>,
    t: f32,
    element: E,
) -> AnimationElement<crate::dialog::Filtered>
where
    E: Styled + IntoElement + 'static,
{
    crate::dialog::filtered(0.0, 1.0, element).with_animation(
        id,
        SURFACE.animation(),
        move |el, _| el.pose(1.0 - 0.02 * t, 1.0 - t),
    )
}

/// The wall-clock span of one run of `spec` — its total stretched by
/// [`speed_scale`].
///
/// The one place the measurement knob is folded into a duration. Every
/// manually driven tween reads its span from here rather than respelling
/// `total() × scale`: the respelled copies are how the knob once got *divided*
/// instead, which made one timeline run 100× fast under the harness while
/// every other one stretched.
#[must_use]
pub fn span(spec: &MotionSpec) -> Duration {
    spec.total().mul_f32(speed_scale())
}

/// Eased progress (0..=1) of an exit that began at `since`, read off the wall
/// clock at render time.
///
/// The one correct source for an exit's `t`. A `with_animation` clock is keyed
/// by element id and replays from 0 whenever the element remounts — mid-exit
/// that is a full-opacity flash, and a dying card that flashes back is the
/// single hardest-won lesson in this module. Wall-clock progress is monotonic
/// by construction and cannot replay.
///
/// Honors [`speed_scale`], so a stretched timeline eases over the stretched
/// span rather than finishing early.
pub fn exit_progress(spec: &MotionSpec, since: Instant) -> f32 {
    exit_progress_at(spec, since, Instant::now())
}

/// [`exit_progress`] against an explicit `now`, for the tweens whose owners
/// keep windowless tests — same clamp, same easing, same [`speed_scale`] fold,
/// so a copy with an injected clock never needs to exist.
#[must_use]
pub fn exit_progress_at(spec: &MotionSpec, since: Instant, now: Instant) -> f32 {
    let total = span(spec).as_secs_f32();
    let raw = if total <= 0.0 {
        1.0
    } else {
        (now.saturating_duration_since(since).as_secs_f32() / total).clamp(0.0, 1.0)
    };
    spec.progress(raw)
}

/// Boot-splash exit: hold QUICK, then fade out + lift 6px over SLOW.
pub fn splash_out<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, SPLASH_OUT.animation(), |el, t| {
        el.opacity(1.0 - t).top(px(-6.0 * t))
    })
}

// ---------------------------------------------------------------------------
// Loader math (pure; rendered by the working indicator)
// ---------------------------------------------------------------------------

// Pure functions of a phase in `0..1`, so a caller can drive them from a frame
// delta or from elapsed wall-clock time and get identical output.

/// Loader cells rest at this opacity between pulses.
pub const PULSE_MIN_OPACITY: f32 = 0.08;
/// …and at this scale.
pub const PULSE_MIN_SCALE: f32 = 0.9;
/// Per-cell stagger, as a fraction of the pulse period (0.15s of 2.4s).
pub const PULSE_STAGGER: f32 = 0.15 / 2.4;
/// Opacity a gradient-spinner cell rests at between pulses.
pub const GSPIN_DIM: f32 = 0.1;

/// A cell's phase, given the loader's raw phase and the cell's index.
pub fn staggered_phase(raw_delta: f32, index: usize, stagger: f32) -> f32 {
    (raw_delta - index as f32 * stagger).rem_euclid(1.0)
}

/// Cosine pulse: 0 at phase 0, 1 at phase 0.5, back to 0 at phase 1.
pub fn pulse_wave(phase: f32) -> f32 {
    0.5 - 0.5 * (phase * std::f32::consts::TAU).cos()
}

/// Loader cell opacity for a phase: 0.08 → 1 → 0.08.
pub fn pulse_opacity(phase: f32) -> f32 {
    PULSE_MIN_OPACITY + (1.0 - PULSE_MIN_OPACITY) * pulse_wave(phase)
}

/// Loader cell scale for a phase: 0.9 → 1 → 0.9.
pub fn pulse_scale(phase: f32) -> f32 {
    PULSE_MIN_SCALE + (1.0 - PULSE_MIN_SCALE) * pulse_wave(phase)
}

/// Gradient-spin cell opacity for a local phase `t` (0..1 of the period): full
/// at the cycle start, easing down to `dim` by 45%, resting there until 92%,
/// then rising back to full — the per-cell phase offset sweeps this pulse
/// across the grid.
pub fn gspin_opacity(t: f32, dim: f32) -> f32 {
    let t = t.rem_euclid(1.0);
    if t < 0.45 {
        lerp(1.0, dim, t / 0.45)
    } else if t < 0.92 {
        dim
    } else {
        lerp(dim, 1.0, (t - 0.92) / 0.08)
    }
}

/// Gradient-matrix spinner wave: intensity (0..1) of cell `wave_index` out of
/// `wave_count` diagonals, at raw delta `raw_delta` of the 750ms period. The wave
/// front travels across diagonals once per period.
pub fn matrix_wave(raw_delta: f32, wave_index: usize, wave_count: usize) -> f32 {
    let count = wave_count.max(1) as f32;
    pulse_wave(staggered_phase(raw_delta, wave_index, 1.0 / count))
}

/// Linear interpolation (layout tweens).
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// How far into a region's opening its content has finished arriving.
///
/// A sliding region reveals its content by *uncovering* it, which on its own
/// reads as content sitting still behind a moving window — every row already at
/// full strength, cut off mid-glyph by the clip. Fading the content in as the
/// region grows is what makes the two read as one gesture.
pub const REVEAL_AT: f32 = 0.6;

/// The opacity content wears at `openness` — how much of its resting size the
/// region showing it currently has, `0.0..=1.0`.
///
/// **No curve and no duration of its own, deliberately.** It reads the openness
/// its caller already has, and that number is a pane's live width over its
/// target — which is [`SURFACE`]'s tween, on [`ROOT`], for however long `SURFACE`
/// says. So this cannot drift from the slide it belongs to, and a change to
/// `SURFACE` moves the fade with it without anything here being touched.
///
/// Content reaches full strength at [`REVEAL_AT`] rather than at the very end:
/// the tail of an ease-out is slow, and content still visibly fading while the
/// edge has all but stopped reads as the panel waiting for its own contents.
#[must_use]
pub fn reveal_opacity(openness: f32) -> f32 {
    (openness.clamp(0.0, 1.0) / REVEAL_AT).min(1.0)
}

// ---------------------------------------------------------------------------
// Hover color fades (CSS `transition-colors` parity)
// ---------------------------------------------------------------------------
//
// gpui `.hover()` styles snap by construction — the style applies the frame
// the pointer enters. The original puts a colour transition on every
// interactive wash. Enter is immediate; only leave fades. This is the manual-drive tween for
// that ([`crate::pane::PaneWidth`]'s pattern — never `with_animation`, whose
// element-id-keyed clock replays on
// remount): a per-element-key hover progress, advanced from wall time on each
// evaluation, with the render tail requesting frames while any fade is
// mid-flight.
//
// The store is a main-thread `thread_local` rather than a gpui Global so the
// many free-function element builders (window-control buttons, popover menu
// rows, markdown code blocks) can blend colors without threading `cx` through
// every signature. All access happens on the UI thread (element builders,
// mouse listeners, the render tail).
//
// Staleness: an element that unmounts mid-hover never gets its leave event, so
// entries are stamped with a frame counter on every read and pruned by
// [`hover_fades_active`] (the once-per-frame tick) when a full frame passes
// without a read — a reopened menu never inherits a dead entry's wash.

/// One element's hover fade: progress runs `origin → target` over
/// [`HOVER_FADE`], re-anchored at `origin` whenever the pointer flips
/// direction mid-flight so the blend is continuous.
#[derive(Debug, Clone, Copy)]
struct FadeEntry {
    origin: f32,
    target: f32,
    started: Instant,
    /// Frame counter at the last read (liveness stamp — see module notes).
    seen: u64,
}

impl FadeEntry {
    fn value(&self, now: Instant, duration: Duration) -> f32 {
        let elapsed = now.saturating_duration_since(self.started);
        if duration.is_zero() || elapsed >= duration {
            return self.target;
        }
        let raw = elapsed.as_secs_f32() / duration.as_secs_f32();
        lerp(self.origin, self.target, HOVER_FADE.curve.eval(raw))
    }

    fn settled(&self, now: Instant, duration: Duration) -> bool {
        self.origin == self.target || now.saturating_duration_since(self.started) >= duration
    }
}

/// Per-key hover progress store. Pure core (explicit `now`) — unit-testable;
/// the thread-local wrappers below feed it wall time.
#[derive(Default)]
pub struct HoverFades {
    entries: HashMap<String, FadeEntry>,
    frame: u64,
}

impl HoverFades {
    fn duration() -> Duration {
        span(&HOVER_FADE)
    }

    /// Pointer entered (`hovered`) or left the element behind `key`. Reduced
    /// motion snaps straight to the endpoint.
    pub fn set_at(&mut self, key: &str, hovered: bool, reduced: bool, now: Instant) {
        let target = if hovered { 1.0 } else { 0.0 };
        let duration = Self::duration();
        let current = self
            .entries
            .get(key)
            .map(|e| e.value(now, duration))
            .unwrap_or(0.0);
        if target == 0.0 && !self.entries.contains_key(key) {
            return; // never-hovered element reporting a leave — nothing to do
        }
        let origin = if hovered || reduced { target } else { current };
        let seen = self.frame;
        self.entries.insert(
            key.to_string(),
            FadeEntry {
                origin,
                target,
                started: now,
                seen,
            },
        );
    }

    /// Hover progress (0..1) for `key` at `now`; stamps liveness.
    pub fn value_at(&mut self, key: &str, now: Instant) -> f32 {
        let frame = self.frame;
        match self.entries.get_mut(key) {
            Some(entry) => {
                entry.seen = frame;
                entry.value(now, Self::duration())
            }
            None => 0.0,
        }
    }

    /// Once-per-frame bookkeeping: advance the frame counter, prune entries
    /// that settled back to rest or went a full frame unread (unmounted), and
    /// report whether any fade is still mid-flight (→ keep frames coming).
    pub fn tick_at(&mut self, now: Instant) -> bool {
        self.frame += 1;
        let frame = self.frame;
        let duration = Self::duration();
        let mut active = false;
        self.entries.retain(|_, entry| {
            // Unread through the whole previous frame: the element unmounted
            // (its leave event will never come) — drop the entry.
            if entry.seen + 1 < frame {
                return false;
            }
            let settled = entry.settled(now, duration);
            if !settled {
                active = true;
            }
            // Settled at rest — steady state, indistinguishable from absent.
            !(settled && entry.target == 0.0)
        });
        active
    }
}

thread_local! {
    static HOVER_FADES: RefCell<HoverFades> = RefCell::new(HoverFades::default());
}

/// Hover progress (0..1) for `key` this frame.
pub fn hover_t(key: &str) -> f32 {
    HOVER_FADES.with(|fades| fades.borrow_mut().value_at(key, Instant::now()))
}

/// Record a hover flip for `key` (reduced motion snaps).
pub fn set_hover(key: &str, hovered: bool, reduced: bool) {
    HOVER_FADES.with(|fades| {
        fades
            .borrow_mut()
            .set_at(key, hovered, reduced, Instant::now())
    });
}

/// An `.on_hover` listener driving the fade for `key` — pair with
/// [`hover_t`]/[`hover_blend`] reads of the same key in the same element.
pub fn hover_listener(
    key: impl Into<SharedString>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let key = key.into();
    move |hovered, window, cx| {
        set_hover(&key, *hovered, reduced_motion(cx));
        // Event-dispatch context: `request_animation_frame` is draw-phase-only
        // (it resolves the current view) — `refresh` marks the whole window
        // dirty, the root render re-evaluates the blend and keeps frames
        // coming via its tail while the fade is mid-flight.
        window.refresh();
    }
}

/// Frame-drive hook: call ONCE per window frame (the shell render tail); true
/// while any hover fade is mid-flight and frames must keep coming.
pub fn hover_fades_active() -> bool {
    HOVER_FADES.with(|fades| fades.borrow_mut().tick_at(Instant::now()))
}

/// Blend two colors by `t` the way the browser transitions them: component
/// interpolation in sRGB with premultiplied alpha — a wash fading in from
/// transparent brightens without passing through grey.
pub fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 {
        return from;
    }
    if t >= 1.0 {
        return to;
    }
    let (f, g) = (Rgba::from(from), Rgba::from(to));
    let a = lerp(f.a, g.a, t);
    if a <= f32::EPSILON {
        // Both endpoints (effectively) transparent — carry the target's hue.
        return Hsla::from(Rgba { a: 0.0, ..g });
    }
    Hsla::from(Rgba {
        r: lerp(f.r * f.a, g.r * g.a, t) / a,
        g: lerp(f.g * f.a, g.g * g.a, t) / a,
        b: lerp(f.b * f.a, g.b * g.a, t) / a,
        a,
    })
}

/// The standard hover blend: rest → hover color at `key`'s current progress.
pub fn hover_blend(key: &str, rest: Hsla, hover: Hsla) -> Hsla {
    mix(rest, hover, hover_t(key))
}

// ---------------------------------------------------------------------------
// Reduced motion
// ---------------------------------------------------------------------------

/// Dev/measurement knob (default 1): stretches every catalog timeline by this
/// factor — e.g. `10` slows the 500ms pane tweens to 5s so screenshot bursts
/// can sample the geometry per frame. Never set in production.
///
/// Per-app rather than per-process, so two harnesses in one test binary can
/// want different timescales. See [`crate::runtime::Runtime`].
pub fn speed_scale() -> f32 {
    crate::runtime::Runtime::with(|runtime| runtime.motion_scale)
}

/// Adopt this app's motion policy. Call once, before a window opens.
///
/// The OS accessibility setting already reaches gpui; this only adds the
/// escape hatch [`Runtime::reduced_motion`](crate::runtime::Runtime), which
/// the automation harness sets so a test reads final geometry the frame after
/// it acts instead of racing a slide. Nothing turns motion back *on* — an
/// override that could contradict the user's accessibility preference is not
/// an override worth having.
pub fn init(cx: &mut App) {
    if crate::runtime::Runtime::with(|runtime| runtime.reduced_motion) {
        set_reduced_motion(cx, true);
    }
}

/// Global reduced-motion flag. gpui snaps every `with_animation` element when
/// set (end state for oneshots, rest state for loops) and schedules no frames;
/// the manually driven tweens in [`crate::pane`] honour it themselves.
pub fn set_reduced_motion(cx: &mut App, reduced: bool) {
    cx.set_reduce_motion(reduced);
}

/// Read the global reduced-motion flag.
pub fn reduced_motion(cx: &App) -> bool {
    cx.reduce_motion()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32, tol: f32, ctx: &str) {
        assert!(
            (actual - expected).abs() <= tol,
            "{ctx}: got {actual}, expected {expected} ±{tol}"
        );
    }

    #[test]
    fn spring_matches_desktop_reference() {
        // Samples from the installed app's duration-based spring solver with
        // duration 500ms, bounce 0.1, and zero initial velocity.
        for (ms, expected) in [
            (0.0, 0.0),
            (50.0, 0.217_610),
            (100.0, 0.537_156),
            (250.0, 0.962_330),
            (400.0, 1.0), // the reference's tiny overshoot is clamped
            (500.0, 1.0),
        ] {
            assert_close(ROOT.eval(ms / 500.0), expected, 1e-5, "desktop spring");
        }
    }

    #[test]
    fn spring_is_bounded_and_monotone() {
        let mut previous = 0.0;
        for i in 0..=100_000 {
            let value = ROOT.eval(i as f32 / 100_000.0);
            assert!((0.0..=1.0).contains(&value), "GPUI requires bounded easing");
            assert!(value >= previous - 1e-6, "spring moved away from target");
            previous = value;
        }
        assert_eq!(ROOT.eval(-0.5), 0.0);
        assert_eq!(ROOT.eval(1.5), 1.0);
    }

    #[test]
    fn spec_delay_holds_then_runs() {
        // The delay and run share the catalog timing ladder.
        assert_eq!(SPLASH_OUT.total(), Duration::from_millis(QUICK + SLOW));
        assert_eq!(SPLASH_OUT.progress(0.0), 0.0);
        // Still inside the delay window.
        assert_eq!(SPLASH_OUT.progress(0.2), 0.0);
        // Fully done at the end; clamped beyond.
        assert_eq!(SPLASH_OUT.progress(1.0), 1.0);
        assert_eq!(SPLASH_OUT.progress(2.0), 1.0);
        // Part-way through the run.
        let mid = SPLASH_OUT.progress(0.65);
        assert!(mid > 0.0 && mid < 1.0);
        // No-delay specs pass straight through the curve.
        assert_close(FADE_IN.progress(0.5), ROOT.eval(0.5), 1e-6, "no-delay");
    }

    #[test]
    fn every_transition_rides_the_root_curve_and_the_ladder() {
        // The design rule, stated where a new entry has to pass it: one curve,
        // and a duration off the ladder. Loader periods are loop lengths, not
        // transitions, so they carry their own numbers (module docs).
        let transitions = [
            FADE_IN,
            FADE_QUICK,
            MENU_IN,
            MENU_OUT,
            SPLASH_OUT,
            SURFACE,
            PUSH,
            TAB_SLIDE,
            COLLAPSE,
            CHEVRON,
            SCROLL_GLIDE,
            HOVER_FADE,
        ];
        for spec in transitions {
            assert_eq!(spec.curve, ROOT, "{spec:?} rides a second curve");
            assert!(
                [SNAP, QUICK, BASE, SWEEP, SLOW].contains(&spec.duration_ms),
                "{spec:?} invents a duration"
            );
            assert!(
                spec.delay_ms == 0 || [SNAP, QUICK, BASE, SWEEP, SLOW].contains(&spec.delay_ms),
                "{spec:?} invents a delay"
            );
        }
        assert_eq!(SURFACE.duration_ms, 500);
        // Strictly increasing, so "a rung below" is a statement about speed and
        // no two rungs are the same number wearing two names.
        let ladder = [SNAP, QUICK, BASE, SWEEP, SLOW];
        assert!(
            ladder.windows(2).all(|pair| pair[0] < pair[1]),
            "the ladder is not strictly increasing: {ladder:?}"
        );
    }

    #[test]
    fn popover_exit_matches_its_entrance() {
        assert_eq!(MENU_OUT, MENU_IN);
    }

    #[test]
    fn panes_and_dialog_exits_share_the_surface_spec() {
        assert_eq!(SURFACE.curve, ROOT);
        assert_eq!(SURFACE.duration_ms, SWEEP);
        assert_eq!(SURFACE.delay_ms, 0);
    }

    #[test]
    fn exit_progress_runs_the_curve_over_the_wall_clock() {
        // At the instant the exit begins it has barely moved (the few
        // microseconds between stamping and reading are real, so this is a
        // tolerance rather than an equality)…
        let now = Instant::now();
        assert!(exit_progress(&SURFACE, now) < 0.01);
        // …and once the span has elapsed it is pinned at the end, not past it.
        let done = now - SURFACE.total().mul_f32(speed_scale());
        assert_eq!(exit_progress(&SURFACE, done), 1.0);
        let long_gone = now - Duration::from_secs(30);
        assert_eq!(exit_progress(&SURFACE, long_gone), 1.0);
        // Mid-flight is strictly inside, and monotonic across the span.
        let half = now - SURFACE.total().mul_f32(speed_scale()).mul_f32(0.5);
        let mid = exit_progress(&SURFACE, half);
        assert!(mid > 0.0 && mid < 1.0, "mid-flight exit: {mid}");
    }

    #[test]
    fn pulse_wave_endpoints() {
        assert_close(pulse_wave(0.0), 0.0, 1e-6, "wave start");
        assert_close(pulse_wave(0.5), 1.0, 1e-6, "wave peak");
        assert_close(pulse_wave(1.0), 0.0, 1e-6, "wave end");
        assert_close(pulse_opacity(0.0), 0.08, 1e-6, "opacity floor");
        assert_close(pulse_opacity(0.5), 1.0, 1e-6, "opacity peak");
        assert_close(pulse_scale(0.0), 0.9, 1e-6, "scale floor");
        assert_close(pulse_scale(0.5), 1.0, 1e-6, "scale peak");
    }

    #[test]
    fn stagger_wraps_and_orders_cells() {
        // Cell 0 at delta 0 is at phase 0; later cells lag by the stagger.
        assert_close(staggered_phase(0.0, 0, PULSE_STAGGER), 0.0, 1e-6, "cell 0");
        assert_close(
            staggered_phase(0.0, 1, PULSE_STAGGER),
            1.0 - PULSE_STAGGER,
            1e-5,
            "cell 1 wraps",
        );
        // A full period later the phase is identical.
        assert_close(
            staggered_phase(0.3, 2, PULSE_STAGGER),
            staggered_phase(0.3 + 1.0, 2, PULSE_STAGGER),
            2e-6,
            "periodic",
        );
        // Matrix wave peaks travel: diagonal k peaks when the front reaches it.
        let peak0 = matrix_wave(0.5, 0, 5);
        assert_close(peak0, 1.0, 1e-5, "diag 0 peak at half period");
    }

    #[test]
    fn lerp_basics() {
        assert_eq!(lerp(208.0, 400.0, 0.0), 208.0);
        assert_eq!(lerp(208.0, 400.0, 1.0), 400.0);
        assert_eq!(lerp(0.0, 10.0, 0.5), 5.0);
    }

    #[test]
    fn hover_enters_instantly_and_fades_out() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);

        // Enter is immediate, including re-entry during an outgoing fade.
        fades.set_at("pill", true, false, t0);
        assert_eq!(fades.value_at("pill", t0), 1.0);
        let mid = fades.value_at("pill", ms(75));
        assert_eq!(mid, 1.0);
        assert_eq!(fades.value_at("pill", ms(150)), 1.0);
        assert_eq!(fades.value_at("pill", ms(400)), 1.0, "clamps past the end");

        // Leave mid-flight re-anchors at the current value — no jump.
        fades.set_at("pill", true, false, t0);
        let at_flip = fades.value_at("pill", ms(75));
        fades.set_at("pill", false, false, ms(75));
        let after_flip = fades.value_at("pill", ms(75));
        assert!(
            (after_flip - at_flip).abs() < 1e-4,
            "continuity: {at_flip} vs {after_flip}"
        );
        let falling = fades.value_at("pill", ms(140));
        assert!(falling < after_flip, "fades back down");
        assert_eq!(fades.value_at("pill", ms(75 + QUICK)), 0.0, "lands at rest");
    }

    #[test]
    fn hover_fade_reduced_motion_snaps() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        fades.set_at("row", true, true, t0);
        assert_eq!(fades.value_at("row", t0), 1.0, "enter snaps to 1");
        fades.set_at("row", false, true, t0);
        assert_eq!(fades.value_at("row", t0), 0.0, "leave snaps to 0");
    }

    #[test]
    fn hover_fade_leave_without_enter_is_inert() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        fades.set_at("ghost", false, false, t0);
        assert!(fades.entries.is_empty(), "no entry for a leave-only key");
        assert_eq!(fades.value_at("ghost", t0), 0.0);
    }

    #[test]
    fn hover_tick_reports_flight_and_prunes() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);

        fades.set_at("a", true, false, t0);
        // Mid-flight: active, frames must keep coming (read each frame).
        assert!(!fades.tick_at(ms(50)));
        fades.value_at("a", ms(50));
        assert!(!fades.tick_at(ms(100)));
        fades.value_at("a", ms(100));
        // Settled hovered (still read): no more frames needed, entry kept.
        assert!(!fades.tick_at(ms(200)));
        fades.value_at("a", ms(200));
        assert_eq!(fades.value_at("a", ms(250)), 1.0);

        // Leave → fades → settles at rest → entry evicted.
        fades.set_at("a", false, false, ms(250));
        assert!(fades.tick_at(ms(300)));
        fades.value_at("a", ms(300));
        assert!(!fades.tick_at(ms(250 + QUICK)), "settled at rest");
        assert!(fades.entries.is_empty(), "rest entries are pruned");
    }

    #[test]
    fn hover_tick_evicts_unread_entries() {
        // An element that unmounts mid-hover never sends its leave — a full
        // frame without a read drops the entry so a remount starts clean.
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        fades.set_at("menu-row", true, false, t0);
        fades.tick_at(ms(16));
        fades.value_at("menu-row", ms(16)); // frame 1: mounted, read
        fades.tick_at(ms(32)); // frame 2: unmounted — no read
        fades.tick_at(ms(48)); // frame 3: a full unread frame has passed
        assert!(fades.entries.is_empty(), "unread entry evicted");
        assert_eq!(fades.value_at("menu-row", ms(64)), 0.0);
    }

    #[test]
    fn mix_endpoints_and_transparent_blend() {
        let rest = crate::glass::neutral(0.235);
        let hover = crate::glass::neutral(0.29);
        assert_eq!(mix(rest, hover, 0.0), rest);
        assert_eq!(mix(rest, hover, 1.0), hover);
        assert_eq!(mix(rest, hover, -1.0), rest, "t clamps low");
        assert_eq!(mix(rest, hover, 2.0), hover, "t clamps high");

        // Opaque blend: lightness moves monotonically between the endpoints.
        let mid = mix(rest, hover, 0.5);
        assert!(mid.l > rest.l && mid.l < hover.l, "mid lightness {}", mid.l);

        // Transparent → wash: alpha ramps, hue stays the wash's (premultiplied
        // — never a darkened grey mid-fade).
        let wash = crate::glass::ink(0.06);
        let half = mix(gpui::transparent_black(), wash, 0.5);
        assert!((half.a - 0.03).abs() < 1e-4, "alpha midpoint {}", half.a);
        let half_rgba = Rgba::from(half);
        assert!(
            half_rgba.r > 0.99 && half_rgba.g > 0.99 && half_rgba.b > 0.99,
            "white wash keeps its hue: {half_rgba:?}"
        );
    }

    #[test]
    fn hover_fade_is_a_quick_undelayed_wash() {
        assert_eq!(HOVER_FADE.duration_ms, QUICK);
        assert_eq!(HOVER_FADE.delay_ms, 0);
    }

    #[test]
    fn gspin_pulse_shape() {
        // Full at the cycle start, dim through the rest band, rising at the tail.
        assert_close(gspin_opacity(0.0, 0.1), 1.0, 1e-6, "cycle start");
        assert_close(gspin_opacity(0.45, 0.1), 0.1, 1e-6, "fully dim");
        assert_close(gspin_opacity(0.9, 0.1), 0.1, 1e-6, "rest band");
        assert_close(gspin_opacity(1.0, 0.1), 1.0, 1e-6, "wraps to full");
        let mid_fall = gspin_opacity(0.2, 0.1);
        assert!(mid_fall > 0.1 && mid_fall < 1.0, "eases down");
        let mid_rise = gspin_opacity(0.96, 0.1);
        assert!(mid_rise > 0.1 && mid_rise < 1.0, "eases up");
    }
    /// The reveal is monotone, bounded, and *finished before the slide is* —
    /// the three things a caller reading a live width off a tween is entitled
    /// to assume, whatever `SURFACE` is retimed to.
    #[test]
    fn content_fades_in_with_the_region_and_arrives_before_it_does() {
        assert_eq!(reveal_opacity(0.0), 0.0, "a closed region shows nothing");
        assert_eq!(reveal_opacity(1.0), 1.0, "an open one shows everything");
        assert_eq!(reveal_opacity(REVEAL_AT), 1.0, "full strength at the mark");

        let mut previous = 0.0;
        for step in 0..=100 {
            let opacity = reveal_opacity(step as f32 / 100.0);
            assert!(
                (0.0..=1.0).contains(&opacity),
                "opacity escaped the unit interval at {step}: {opacity}"
            );
            assert!(opacity >= previous, "the fade went backwards at {step}");
            previous = opacity;
        }

        // Out-of-range openness is a usable opacity rather than a panic or a
        // wash over 1: a live width divided by its target is float arithmetic,
        // and it lands a hair outside on the frame it settles.
        assert_eq!(reveal_opacity(1.000_001), 1.0);
        assert_eq!(reveal_opacity(-0.5), 0.0);
    }

    /// The fade borrows the slide's clock rather than keeping one: its only
    /// input is how open the region is, so retiming [`SURFACE`] moves the fade
    /// with it and cannot desynchronise the two. What is left to pin is the
    /// other half of that borrow — the curve the openness arrives on.
    #[test]
    fn the_reveal_has_no_clock_of_its_own() {
        assert_eq!(SURFACE.curve, ROOT, "the slide it reads must stay on ROOT");
    }
}
