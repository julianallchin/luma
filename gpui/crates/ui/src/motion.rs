//! Ported from zeron (MIT, © 2026 Wing) — crates/ui/src/motion.rs
//!
//! Animation kit — one spring, four durations, and the helpers that ride them
//! over gpui [`Animation`]/[`AnimationExt`].
//!
//! # One curve
//!
//! Everything that moves eases on [`ROOT`]. A second curve would be a second
//! motion language, and the app would read as two apps sharing a window: the
//! only thing that distinguishes a menu pop from a panel slide is how far and
//! for how long, never *how*.
//!
//! Durations come from the ladder — [`SNAP`], [`QUICK`], [`BASE`], [`SWEEP`] —
//! for the same reason the greys do: a new animation picks a rung rather than
//! inventing a number, and another rung is a design decision, not a literal.
//! Loader *periods* ([`PULSE`], [`BREATHE`], [`GRADIENT_SPIN`]) are not on the
//! ladder — they are the length of a loop, not of a transition.
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

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationElement, App, ElementId, EntityId, Global, Hsla, IntoElement, Rgba, Styled,
};

pub use gpui::AnimationExt;

// ---------------------------------------------------------------------------
// Pulse clock — throttled drive for the repeating loaders
// ---------------------------------------------------------------------------

/// Repeat-tick interval for the pulse/spinner loaders (~30fps).
///
/// Do not drive loaders with a repeating gpui `with_animation`: it requests a
/// redraw every display frame while mounted, which holds the whole window at
/// display rate. A shared 30fps clock looks the same for these chunky cell
/// waves at a quarter of the redraws, and a window with no spinner mounted
/// schedules nothing at all.
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

/// One catalog entry: duration + curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSpec {
    pub duration_ms: u64,
    pub curve: Spring,
}

impl MotionSpec {
    pub const fn new(duration_ms: u64, curve: Spring) -> Self {
        Self { duration_ms, curve }
    }

    /// Wall-clock span of the timeline.
    pub fn total(&self) -> Duration {
        Duration::from_millis(self.duration_ms)
    }

    /// Eased progress (0..1) for a raw timeline delta (0..1 across [`total`](Self::total)).
    /// Pure — unit-testable without a window.
    pub fn progress(&self, raw_delta: f32) -> f32 {
        if self.duration_ms == 0 {
            return 1.0;
        }
        self.curve.eval(raw_delta.clamp(0.0, 1.0))
    }

    /// A oneshot gpui [`Animation`] for this spec.
    /// Wall-clock span honors [`speed_scale`] (measurement knob).
    pub fn animation(&self) -> Animation {
        let spec = *self;
        Animation::new(span(&spec)).with_easing(move |d| spec.progress(d))
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

// -- the catalog --------------------------------------------------------------

/// Opacity-only fade.
pub const FADE_QUICK: MotionSpec = MotionSpec::new(QUICK, ROOT);
/// Popover entrance, moving away from its trigger.
pub const MENU_IN: MotionSpec = MotionSpec::new(QUICK, ROOT);
/// Popover exit uses the same timing and spring as its entrance.
pub const MENU_OUT: MotionSpec = MENU_IN;
/// Shared spring timing for panes, dialog route morphs, and dialog exits.
pub const SURFACE: MotionSpec = MotionSpec::new(SWEEP, ROOT);
/// Dialogs open more quickly while retaining the same spring shape.
pub const DIALOG_IN: MotionSpec = MotionSpec::new(BASE, ROOT);
/// Tab drag-reorder sliding transforms.
pub const TAB_SLIDE: MotionSpec = MotionSpec::new(QUICK, ROOT);
/// Per-row collapse (height).
pub const COLLAPSE: MotionSpec = MotionSpec::new(BASE, ROOT);
/// The temporal blend every interactive hover wash rides.
pub const HOVER_FADE: MotionSpec = MotionSpec::new(QUICK, ROOT);
/// Loader pulse period (a loop length, not a transition — see the module docs).
pub const PULSE: MotionSpec = MotionSpec::new(2400, ROOT);
/// The startup mark's breath — slower than [`PULSE`], because it is the
/// only thing on the screen and a quick beat there reads as urgency.
pub const BREATHE: MotionSpec = MotionSpec::new(3000, ROOT);
/// Gradient matrix spinner wave period.
pub const GRADIENT_SPIN: MotionSpec = MotionSpec::new(750, ROOT);

// ---------------------------------------------------------------------------
// Element helpers (paint-layer entrances/exits)
// ---------------------------------------------------------------------------

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
/// clock at render time — or of an entrance that must play once, when its
/// owner was made, however often the element is unmounted and mounted again.
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

// ---------------------------------------------------------------------------
// Loader math (pure; rendered by the working indicator)
// ---------------------------------------------------------------------------

// Pure functions of a phase in `0..1`, so a caller can drive them from a frame
// delta or from elapsed wall-clock time and get identical output.

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
// Hover color fades
// ---------------------------------------------------------------------------
//
// gpui's `.hover()` styles snap by construction. The original puts a colour
// transition on every interactive wash: enter is immediate, only leave fades.
// That fade lives in gpui itself ([`gpui::InteractiveElement::hover_fade`]),
// in each element's own state: it lives and dies with the element, and while
// it runs it redraws only the view that owns the element. [`FadeHover`] gives
// it this catalog's timing.
//
// It replaced a store keyed by strings outside gpui, where an entry stayed
// alive only while something re-rendered and read it. A cached region reused
// on a stage-only frame reads nothing, so a hovered row lost its wash the
// moment it got it.

/// The catalog's hover fade on any interactive element: pair with a
/// `.hover(…)` style, and give the element an `.id(…)` — without one the
/// hover style snaps.
pub trait FadeHover: gpui::InteractiveElement + Sized {
    /// Fade out of the hover style on leave, over [`HOVER_FADE`].
    fn fade_hover(self) -> Self {
        let curve = HOVER_FADE.curve;
        self.hover_fade(span(&HOVER_FADE), move |t| curve.eval(t))
    }
}

impl<E: gpui::InteractiveElement> FadeHover for E {}

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
    fn every_transition_rides_the_root_curve_and_the_ladder() {
        // The design rule, stated where a new entry has to pass it: one curve,
        // and a duration off the ladder. Loader periods are loop lengths, not
        // transitions, so they carry their own numbers (module docs).
        let transitions = [
            FADE_QUICK, MENU_IN, MENU_OUT, SURFACE, TAB_SLIDE, COLLAPSE, HOVER_FADE,
        ];
        for spec in transitions {
            assert_eq!(spec.curve, ROOT, "{spec:?} rides a second curve");
            assert!(
                [SNAP, QUICK, BASE, SWEEP].contains(&spec.duration_ms),
                "{spec:?} invents a duration"
            );
        }
        // Strictly increasing, so "a rung below" is a statement about speed and
        // no two rungs are the same number wearing two names.
        let ladder = [SNAP, QUICK, BASE, SWEEP];
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
    }

    #[test]
    fn stagger_wraps_and_orders_cells() {
        const STAGGER: f32 = 0.15 / 2.4;
        // Cell 0 at delta 0 is at phase 0; later cells lag by the stagger.
        assert_close(staggered_phase(0.0, 0, STAGGER), 0.0, 1e-6, "cell 0");
        assert_close(
            staggered_phase(0.0, 1, STAGGER),
            1.0 - STAGGER,
            1e-5,
            "cell 1 wraps",
        );
        // A full period later the phase is identical.
        assert_close(
            staggered_phase(0.3, 2, STAGGER),
            staggered_phase(0.3 + 1.0, 2, STAGGER),
            2e-6,
            "periodic",
        );
    }

    #[test]
    fn lerp_basics() {
        assert_eq!(lerp(208.0, 400.0, 0.0), 208.0);
        assert_eq!(lerp(208.0, 400.0, 1.0), 400.0);
        assert_eq!(lerp(0.0, 10.0, 0.5), 5.0);
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
