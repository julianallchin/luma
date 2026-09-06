//! Cross-annotation compositing — the blend fold.
//!
//! This is the "compositor built in" piece: a Rust-level fold, *not* an IR op.
//! Annotations have independent spans, z-order, and lifetimes (they enter/leave
//! the timeline, and in perform mode the DJ adds/removes/reorders them live), so
//! compositing is a fold over independent per-plan [`eval`](super::eval) results
//! rather than a static meta-plan. The IR's `OpKind::Blend` stays reserved for
//! within-graph branch blending.
//!
//! The blend math is ported verbatim from the legacy `compositor.rs` /
//! `engine::composite_layer_frame` so the look is preserved bit-for-bit through
//! the engine swap. The one structural change: legacy carried a per-channel
//! `Option<Series>` "set mask" (a layer that only drives dimmer leaves color
//! untouched so the base shows through). Eval always emits a *full*
//! [`UniverseState`], so the set mask comes from the plan's [`OutputBinding`]
//! instead — only capabilities the plan actually binds get blended onto the base.

use crate::eval::{BlendMode, OutputBinding};
use crate::models::universe::{PrimitiveState, UniverseState};
use std::collections::HashSet;

pub use luma_patterns::{blend_color, blend_value};

/// A fresh, fully-unset base frame: dimmer 0, color white, no strobe, speed fast.
/// Matches the engine defaults so an empty composite reads as blackout.
pub fn blank_frame() -> UniverseState {
    UniverseState {
        primitives: std::collections::HashMap::new(),
    }
}

/// Default state for a primitive that the base doesn't yet hold (mirrors
/// `assemble`'s unset defaults in [`super::eval`]).
fn default_primitive() -> PrimitiveState {
    PrimitiveState {
        dimmer: 0.0,
        color: [1.0, 1.0, 1.0],
        strobe: 0.0,
        position: [0.0, 0.0],
        speed: 1.0,
    }
}

/// Composite one annotation's evaluated frame (`top`) onto `base` in place.
///
/// `bindings` is the plan's [`OutputBinding`] — the set mask. Only capabilities
/// the plan drives are blended; the rest of `base` shows through (exactly the
/// legacy `Option<Series>` semantics). `intensity` scales dimmer (deck volume /
/// group / master in perform mode). `allowed`, when `Some`, restricts the blend
/// to those primitive ids (group-targeted cues); `None` means all.
///
/// Channel rules match legacy `composite_layer_frame`:
/// - dimmer / strobe: scalar `blend_value`
/// - color: `blend_color` using the *top dimmer* (× intensity) as opacity
/// - position: winner-takes-all when the top drives it
/// - speed: binary (threshold 0.5)
pub fn composite_frame(
    base: &mut UniverseState,
    top: &UniverseState,
    bindings: &OutputBinding,
    mode: BlendMode,
    intensity: f32,
    allowed: Option<&HashSet<&str>>,
) {
    let sets_dimmer = bindings.dimmer.is_some();
    let sets_color = bindings.color.is_some();
    let sets_strobe = bindings.strobe.is_some();
    let sets_position = bindings.position.is_some();
    let sets_speed = bindings.speed.is_some();

    for (id, tp) in &top.primitives {
        if let Some(set) = allowed {
            // Match on the fixture id (prefix before ':head'), like legacy target filtering.
            let fixture = id.split(':').next().unwrap_or(id.as_str());
            if !set.contains(id.as_str()) && !set.contains(fixture) {
                continue;
            }
        }
        let scaled_dimmer = (tp.dimmer * intensity).clamp(0.0, 1.0);

        match base.primitives.get_mut(id) {
            // FIRST contributor to this primitive: take its bound capabilities
            // directly. Blending over the white default base would mix white into
            // a dim color (a dark red → pink → desaturated). Only *overlapping*
            // annotations blend (the `Some` arm). Matches legacy
            // `composite_cue_onto_universe` (insert-direct if not present).
            None => {
                let mut p = default_primitive();
                if sets_color {
                    p.color = tp.color;
                }
                if sets_dimmer {
                    p.dimmer = scaled_dimmer;
                }
                if sets_strobe {
                    p.strobe = tp.strobe.clamp(0.0, 1.0);
                }
                if sets_position {
                    p.position = tp.position;
                }
                if sets_speed {
                    p.speed = if tp.speed > 0.5 { 1.0 } else { 0.0 };
                }
                base.primitives.insert(id.clone(), p);
            }
            // Already lit by a lower-z annotation: blend on top. Color uses the
            // top's dimmer (× intensity) as opacity so dark layers add little.
            Some(bp) => {
                if sets_color {
                    bp.color = blend_color(bp.color, tp.color, scaled_dimmer, mode);
                }
                if sets_dimmer {
                    bp.dimmer = blend_value(bp.dimmer, scaled_dimmer, mode).clamp(0.0, 1.0);
                }
                if sets_strobe {
                    bp.strobe =
                        blend_value(bp.strobe, tp.strobe.clamp(0.0, 1.0), mode).clamp(0.0, 1.0);
                }
                if sets_position {
                    bp.position = tp.position;
                }
                if sets_speed {
                    bp.speed = if tp.speed > 0.5 { 1.0 } else { 0.0 };
                }
            }
        }
    }
}
