//! Cross-annotation compositing — the blend fold.
//!
//! This is the "compositor built in" piece: a Rust-level fold, *not* an IR op.
//! Annotations have independent spans, z-order, and lifetimes (they enter/leave
//! the timeline, and in perform mode the DJ adds/removes/reorders them live), so
//! compositing is a fold over independent per-plan [`eval`](super::eval) results
//! over the canonical graph outputs.
//!
//! Nothing under a head is zero light: no clip on it, or a lower clip that
//! left it dark. Every layer blends against that light with the same math,
//! so a black head under an add or screen layer changes nothing and a
//! multiply over nothing stays nothing (see [`blend_light`]). Eval always
//! emits a *full* [`UniverseState`], so the set mask comes from the plan's
//! [`OutputBinding`]: only capabilities the plan actually binds get blended.

use crate::eval::{BlendMode, OutputBinding};
use crate::models::universe::{HeadAim, PrimitiveState, UniverseState};
use luma_patterns::{blend_aim, blend_light, blend_value};

/// A fresh, empty base frame: no head holds light yet.
pub fn blank_frame() -> UniverseState {
    UniverseState {
        primitives: std::collections::HashMap::new(),
    }
}

/// A head that no layer has written: no light, no strobe, speed fast.
fn nothing() -> PrimitiveState {
    PrimitiveState {
        dimmer: 0.0,
        color: [1.0, 1.0, 1.0],
        strobe: 0.0,
        position: [0.0, 0.0],
        speed: 1.0,
        aim: None,
    }
}

/// Composite one annotation's evaluated frame (`top`) onto `base` in place.
///
/// `bindings` is the plan's [`OutputBinding`] — the set mask. Only capabilities
/// the plan drives are blended; the rest of `base` shows through.
///
/// - color and dimmer: [`blend_light`], against no light where `base` has no head
/// - strobe: scalar `blend_value`, against 0 where `base` has no head
/// - position: winner-takes-all when the top drives it
/// - speed: binary (threshold 0.5)
/// - aim: whatever the mode, blends toward the aim under it by alpha along
///   the shortest arc ([`blend_aim`]). With no aim under it, the clip blends
///   from the head's home: its alpha becomes the aim's weight, and the
///   solver aims at `slerp(home, direction, weight)`.
pub fn composite_frame(
    base: &mut UniverseState,
    top: &UniverseState,
    bindings: &OutputBinding,
    mode: BlendMode,
) {
    for (id, tp) in &top.primitives {
        if !base.primitives.contains_key(id) {
            base.primitives.insert(id.clone(), nothing());
        }
        let bp = base
            .primitives
            .get_mut(id)
            .expect("the head was inserted above");
        if bindings.color || bindings.dimmer {
            (bp.color, bp.dimmer) = blend_light(bp.color, bp.dimmer, tp.color, tp.dimmer, mode);
        }
        if bindings.strobe {
            bp.strobe = blend_value(bp.strobe, tp.strobe.clamp(0.0, 1.0), mode).clamp(0.0, 1.0);
        }
        if bindings.position {
            bp.position = tp.position;
        }
        if bindings.speed {
            bp.speed = if tp.speed > 0.5 { 1.0 } else { 0.0 };
        }
        if let (true, Some(top)) = (bindings.aim, tp.aim) {
            bp.aim = blend_aim(bp.aim.map(HeadAim::to_aim), top.to_aim()).map(HeadAim::from_aim);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::eval::{lighting::compile_clip, Arena, CompiledAnnotation, Scene, Scope};
    use luma_patterns as p;
    use std::sync::Arc;

    /// One Wash clip on one head over beats 0–4 (one beat per second).
    fn wash(
        color: [f64; 3],
        brightness: f64,
        alpha: f64,
        mode: p::BlendMode,
        z: i64,
    ) -> CompiledAnnotation {
        let mut clip = p::presets().preset("Wash").unwrap().clip(0.0, 4.0);
        clip.inputs.insert("color".into(), p::Value::Color(color));
        clip.inputs
            .insert("brightness".into(), p::Value::Proportion(brightness));
        clip.inputs
            .insert("alpha".into(), p::Value::Proportion(alpha));
        compile(clip, mode, z)
    }

    fn compile(clip: p::Clip, mode: p::BlendMode, z: i64) -> CompiledAnnotation {
        let cells = vec![p::Cell {
            id: "head".into(),
            group: "wash".into(),
            world: [0.0; 3],
            uvz: [0.0; 3],
        }];
        let prepared = p::PreparedGraph::new(
            &p::standard_library(),
            &clip.graph,
            &clip.inputs,
            p::Frame {
                features: None,
                cells: &cells,
                beat: 0.0,
                clip_start: 0.0,
                clip_duration: 4.0,
                seed: 0,
            },
        )
        .unwrap();
        let clock = p::BeatTimeline::new(vec![0.0, 1.0, 2.0, 3.0, 4.0], 0.0).unwrap();
        let plan = compile_clip(&clip, clock, cells, prepared, "lighting").unwrap();
        CompiledAnnotation {
            span: plan.span,
            plan: Arc::new(plan),
            z_index: z,
            blend_mode: mode,
        }
    }

    /// One `aim@1` Position clip on the head over beats 0–4, aimed at
    /// `direction` at `alpha`.
    fn aim(direction: [f64; 3], alpha: f64, z: i64) -> CompiledAnnotation {
        let mut clip = p::presets().preset("Position").unwrap().clip(0.0, 4.0);
        clip.inputs
            .insert("direction".into(), p::Value::Vector(direction));
        clip.inputs
            .insert("alpha".into(), p::Value::Proportion(alpha));
        compile(clip, p::BlendMode::Replace, z)
    }

    /// The head's light (color × dimmer) at beat 1; no head is no light.
    fn light(layers: Vec<CompiledAnnotation>) -> [f32; 3] {
        let frame = Scene::new(layers).render(&[1.0], Scope::Composite, &mut Arena::default());
        frame[0]
            .primitives
            .get("head")
            .map(|h| h.color.map(|c| c * h.dimmer))
            .unwrap_or([0.0; 3])
    }

    fn close(a: [f32; 3], b: [f32; 3]) {
        assert!(
            a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5),
            "{a:?} != {b:?}"
        );
    }

    const RED: [f64; 3] = [1.0, 0.0, 0.0];
    const BLUE: [f64; 3] = [0.0, 0.0, 1.0];

    fn head(layers: Vec<CompiledAnnotation>) -> crate::models::universe::PrimitiveState {
        Scene::new(layers).render(&[1.0], Scope::Composite, &mut Arena::default())[0].primitives
            ["head"]
            .clone()
    }

    #[test]
    fn aim_clips_blend_by_alpha_along_the_shortest_arc() {
        const REST: [f64; 3] = [0.0, 0.766, -0.643];
        const RIGHT: [f64; 3] = [1.0, 0.0, 0.0];
        let blended = head(vec![aim(REST, 1.0, 0), aim(RIGHT, 0.25, 1)])
            .aim
            .unwrap();
        let expected = p::aim::slerp(REST, RIGHT, 0.25).map(|v| v as f32);
        assert_eq!(blended.weight, 1.0);
        assert!(
            blended
                .direction
                .iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 1e-5),
            "{blended:?}"
        );
        // Over no aim, the clip blends from home: alpha is the weight.
        let alone = head(vec![aim(RIGHT, 0.25, 0)]).aim.unwrap();
        assert_eq!(alone.weight, 0.25);
        // Alpha 0 is no clip; a head with no aim clip has no aim.
        let under = head(vec![aim(REST, 1.0, 0)]).aim;
        assert_eq!(head(vec![aim(REST, 1.0, 0), aim(RIGHT, 0.0, 1)]).aim, under);
        assert_eq!(
            head(vec![wash(RED, 1.0, 1.0, p::BlendMode::Replace, 0)]).aim,
            None
        );
        // Aim and light composite apart.
        let both = head(vec![
            wash(RED, 1.0, 1.0, p::BlendMode::Replace, 0),
            aim(RIGHT, 1.0, 1),
        ]);
        assert_eq!(both.dimmer, 1.0);
        assert_eq!(both.aim.unwrap().direction, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn add_and_screen_over_black_equal_over_nothing() {
        for mode in [p::BlendMode::Add, p::BlendMode::Screen] {
            let top = || wash(BLUE, 0.5, 1.0, mode, 1);
            let on_nothing = light(vec![top()]);
            close(on_nothing, [0.0, 0.0, 0.5]);
            // A dark red clip under the layer: brightness 0 or alpha 0.
            close(
                light(vec![wash(RED, 0.0, 1.0, p::BlendMode::Replace, 0), top()]),
                on_nothing,
            );
            close(
                light(vec![wash(RED, 1.0, 0.0, p::BlendMode::Replace, 0), top()]),
                on_nothing,
            );
        }
    }

    #[test]
    fn multiply_over_nothing_is_nothing() {
        close(
            light(vec![wash([1.0; 3], 1.0, 1.0, p::BlendMode::Multiply, 0)]),
            [0.0; 3],
        );
        close(
            light(vec![
                wash(RED, 0.8, 1.0, p::BlendMode::Replace, 0),
                wash([1.0; 3], 0.5, 1.0, p::BlendMode::Multiply, 1),
            ]),
            [0.4, 0.0, 0.0],
        );
    }

    #[test]
    fn replace_black_over_a_lit_wash_paints_black() {
        close(
            light(vec![
                wash(RED, 1.0, 1.0, p::BlendMode::Replace, 0),
                wash(BLUE, 0.0, 1.0, p::BlendMode::Replace, 1),
            ]),
            [0.0; 3],
        );
    }

    #[test]
    fn alpha_zero_is_absent() {
        let lit = || wash(RED, 0.6, 1.0, p::BlendMode::Replace, 0);
        for mode in [
            p::BlendMode::Add,
            p::BlendMode::Screen,
            p::BlendMode::Subtract,
        ] {
            close(
                light(vec![lit(), wash(BLUE, 1.0, 0.0, mode, 1)]),
                light(vec![lit()]),
            );
        }
        // Under a layer, a clip at alpha 0 is no clip.
        for mode in [
            p::BlendMode::Add,
            p::BlendMode::Screen,
            p::BlendMode::Multiply,
        ] {
            let top = || wash(BLUE, 0.5, 1.0, mode, 1);
            close(
                light(vec![wash(RED, 1.0, 0.0, p::BlendMode::Replace, 0), top()]),
                light(vec![top()]),
            );
        }
    }
}
