//! Cone geometry: one continuous luminaire model, one source of truth.
//!
//! Port of `src/features/visualizer/lib/luminaire.ts`. A fixture type is an
//! opening angle on a single zoom axis; concentration (lumens per solid angle)
//! derives brightness, throw, edge hardness and scatter anisotropy. The angle
//! comes from the definition's `Physical.Lens`; the per-kind table is a
//! fallback for definitions that omit it, and it is the only such table.
//!
//! Spec §3.2 places this beside `head_world_position` in the Tauri crate once
//! the port lands. It lives here for now because nothing else consumes it yet.

use crate::scene_desc::Definition;
use fixture_kinematics::{aim, Articulation, Mount};
use glam::Vec3;

/// A fixture's optics, reduced to the two numbers the cone model needs.
#[derive(Debug, Clone, Copy)]
pub struct Luminaire {
    /// Full field angle (deg) — the fixture's opening.
    pub field_angle_deg: f32,
    /// Relative lumen output; 1 = a stock moving-head lamp.
    pub lumens: f32,
}

/// Per-pixel emitter of a procedural LED bar / matrix — not a lensed fixture.
pub const PIXEL: Luminaire = Luminaire {
    field_angle_deg: 60.0,
    lumens: 0.6,
};

/// Quoted lens degrees are the *beam* angle (the 50%-intensity core); the
/// visible field extends to roughly twice that.
const FIELD_PER_BEAM: f32 = 2.0;

/// Openings outside this range are not physical: they break the cone math, and
/// they are not a lens anybody could look through either.
fn clamp_opening(deg: f32) -> f32 {
    deg.clamp(4.0, 160.0)
}

/// Fallback *beam* angles, used only when `Physical.Lens` is missing or blank.
fn fallback(kind: Option<ModelKind>) -> (f32, f32) {
    match kind {
        Some(ModelKind::MovingHead) => (18.0, 1.0),
        Some(ModelKind::Scanner) => (16.0, 1.0),
        Some(ModelKind::Strobe) => (78.0, 3.0),
        // Pars, and anything unrecognised, land on the library median.
        _ => (25.0, 1.0),
    }
}

/// QLC+ writes `DegreesMin="0" DegreesMax="0"` for "unknown", so zero means
/// absent rather than a zero-degree beam.
fn lens_beam_angle(def: &Definition) -> Option<f32> {
    let lens = def.physical.as_ref()?.lens.as_ref()?;
    let lo = if lens.degrees_min > 0.0 {
        lens.degrees_min
    } else {
        lens.degrees_max
    };
    let hi = if lens.degrees_max > 0.0 {
        lens.degrees_max
    } else {
        lens.degrees_min
    };
    (lo > 0.0).then(|| f32::midpoint(lo, hi))
}

/// The one answer to "how wide is this fixture's **beam**" — the 50%-intensity
/// core, in degrees.
///
/// The lens block when the definition has one, the class median otherwise (25
/// degrees for a par and for anything unrecognised). Total over a definition
/// the catalogue no longer has, because a venue outlives a fixture bundle and
/// the question still has to have an answer. Public because a second reading of
/// `Physical.Lens` anywhere else would be a second answer.
#[must_use]
pub fn beam_angle_deg(def: Option<&Definition>, kind: Option<ModelKind>) -> f32 {
    clamp_opening(
        def.and_then(lens_beam_angle)
            .unwrap_or_else(|| fallback(kind).0),
    )
}

/// The one answer to "how wide is this fixture's cone".
#[must_use]
pub fn luminaire_for(def: &Definition, kind: Option<ModelKind>) -> Luminaire {
    Luminaire {
        field_angle_deg: clamp_opening(beam_angle_deg(Some(def), kind) * FIELD_PER_BEAM),
        lumens: fallback(kind).1,
    }
}

/// Cone geometry derived from an opening angle.
#[derive(Debug, Clone, Copy)]
pub struct Cone {
    /// Cosine of the half-angle where intensity is 50%.
    pub cos_beam: f32,
    /// Cosine of the half-angle where the profile reaches zero.
    pub cos_field: f32,
    /// Throw distance, in metres.
    pub range: f32,
    /// 0 for a hard beam, 1 for a near-isotropic wash.
    pub wash: f32,
    /// Intensity multiplier: lumens x solid-angle concentration.
    pub gain: f32,
}

fn cone_solid_angle(full_angle_deg: f32) -> f32 {
    2.0 * std::f32::consts::PI * (1.0 - (full_angle_deg.to_radians() / 2.0).cos())
}

fn smoothstep01(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Cone geometry for one opening. Same energy through a smaller solid angle is
/// hotter, whiter and throws further; everything below follows from that.
#[must_use]
pub fn cone_from_opening(l: Luminaire) -> Cone {
    // Concentration reference: a 30 degree spot has gain 1.5 and 12 m of throw.
    let reference = cone_solid_angle(30.0);
    let field_deg = clamp_opening(l.field_angle_deg);
    // Same energy through a smaller solid angle = hotter, whiter, longer throw.
    let concentration = reference / cone_solid_angle(field_deg);
    // Wide openings scatter near-isotropically and develop a soft shoulder;
    // the beam:field ratio narrows continuously as the cone opens.
    let wash = smoothstep01(20.0, 80.0, field_deg);
    let beam_ratio = 0.6 - 0.25 * wash;
    let half = field_deg.to_radians() / 2.0;
    Cone {
        cos_field: half.cos(),
        cos_beam: (half * beam_ratio).cos(),
        range: (12.0 * concentration.sqrt()).clamp(3.0, 18.0),
        wash,
        gain: 1.5 * l.lumens * concentration.clamp(0.1, 6.0),
    }
}

/// Which bundled mesh (and which fallback cone) a definition resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// Fixed-lens wash / colour changer.
    Par,
    /// Pan-and-tilt head.
    MovingHead,
    /// Mirror scanner.
    Scanner,
    /// Blinder / strobe.
    Strobe,
    /// Haze machine — no beam, scales global haze density.
    Hazer,
    /// Fog machine — as `Hazer`.
    Smoke,
}

impl ModelKind {
    /// Every kind, so a caller that has to touch each bundled mesh — measuring
    /// them for [`crate::catalog::clamp_standoff`] — cannot miss one when a
    /// seventh is added.
    pub const ALL: [Self; 6] = [
        Self::Par,
        Self::MovingHead,
        Self::Scanner,
        Self::Strobe,
        Self::Hazer,
        Self::Smoke,
    ];

    /// Relative path under `resources/meshes/qlc/`.
    #[must_use]
    pub fn mesh(self) -> &'static str {
        match self {
            Self::Par => "par.glb",
            Self::MovingHead => "moving_head.glb",
            Self::Scanner => "scanner.glb",
            Self::Strobe => "strobe.glb",
            Self::Hazer => "hazer.glb",
            Self::Smoke => "smoke.glb",
        }
    }

    /// Hazers and smoke machines emit haze, not a beam.
    #[must_use]
    pub fn emits_beam(self) -> bool {
        !matches!(self, Self::Hazer | Self::Smoke)
    }

    /// Face point-light intensity — lights the housing from behind the lens.
    #[must_use]
    pub fn face_light_intensity(self) -> f32 {
        match self {
            Self::MovingHead | Self::Scanner => 50.0,
            Self::Strobe => 40.0,
            // Pars and the beamless kinds share the default.
            _ => 30.0,
        }
    }

    /// Distance from the head origin down to the face light, from
    /// `static-fixture.tsx`'s `-(originOffset + 0.3)`.
    #[must_use]
    pub fn face_light_offset(self) -> f32 {
        let origin_offset = match self {
            Self::Par => 0.1,
            Self::MovingHead | Self::Scanner => 0.15,
            Self::Strobe => 0.05,
            _ => 0.12,
        };
        origin_offset + 0.3
    }
}

/// The world-space direction a fixture's beam leaves along, or zero for a
/// fixture that has no beam.
///
/// The one definition of "where is this pointing", shared by the frame builder
/// that draws the cone and by [`Scene::framing`](crate::scene_desc::Scene::framing)
/// that frames it — two answers here would be a camera fitted to a beam the
/// renderer does not draw, which is how a pixel bar aimed along its own length
/// pulled a club's extent six metres sideways.
///
/// Rest is the **mount normal** and nothing else: hung square, every fixture
/// fires straight down, bar and mover alike. A bar used to be special-cased to
/// its housing's `+depth` face, which is where the third of this codebase's
/// three rest conventions lived; the housing's front face is now turned onto the
/// mount normal once, where the bar is drawn, instead of being re-derived here.
///
/// `position` is `[pan, tilt]` in degrees, or `None` for a head with no pinned
/// state. Pixel bars have no pan or tilt, so theirs is ignored rather than
/// refused. A definition that is absent from the catalogue, or one whose type
/// emits haze rather than light, has no direction at all.
#[must_use]
pub fn beam_direction(def: Option<&Definition>, rot: [f32; 3], position: Option<[f32; 2]>) -> Vec3 {
    let Some(def) = def else {
        return Vec3::ZERO;
    };
    let articulation = if is_procedural(def) {
        Articulation::REST
    } else if model_kind(def).is_some_and(ModelKind::emits_beam) {
        let [pan, tilt] = position.unwrap_or([0.0, 0.0]);
        Articulation::from_degrees(pan, tilt)
    } else {
        return Vec3::ZERO;
    };
    crate::coords::world_from_data(aim(&Mount::from_stored(Vec3::ZERO, rot), &articulation))
}

/// LED bars and matrices are drawn from their layout rather than from a mesh.
///
/// Fuzzy for the same reason [`model_kind`] is: `Type` is free text a bundle
/// author wrote, and the two exact strings QLC+ happens to ship are not the
/// whole of what a bar is called. A definition that matched neither the exact
/// pair here nor any arm of `model_kind` drew **no body at all** — arrows and a
/// cone leaving nothing — which is also how a housing buried in a truss looks,
/// so one bug hid the other.
#[must_use]
pub fn is_procedural(def: &Definition) -> bool {
    if def.kind == "LED Bar (Pixels)" || def.kind == "LED Bar (Beams)" {
        return true;
    }
    let lower = def.kind.to_lowercase();
    (lower.contains("bar") || lower.contains("matrix") || lower.contains("pixel"))
        && model_kind_exact(def).is_none()
}

/// The exact half of [`model_kind`]'s table, so [`is_procedural`] can ask
/// "is this already a modelled kind by name" without the fuzzy arms — which
/// would claim a "LED Bar" for `Par` on the substring alone.
fn model_kind_exact(def: &Definition) -> Option<ModelKind> {
    match def.kind.as_str() {
        "Color Changer" | "Dimmer" => Some(ModelKind::Par),
        "Moving Head" => Some(ModelKind::MovingHead),
        "Scanner" => Some(ModelKind::Scanner),
        "Strobe" => Some(ModelKind::Strobe),
        "Hazer" => Some(ModelKind::Hazer),
        "Smoke" => Some(ModelKind::Smoke),
        _ => None,
    }
}

/// Which bundled mesh a definition's `Type` selects. Port of
/// `getModelForFixture`, exact-match first and then the same fuzzy fallbacks.
#[must_use]
pub fn model_kind(def: &Definition) -> Option<ModelKind> {
    if let Some(exact) = model_kind_exact(def) {
        return Some(exact);
    }
    let lower = def.kind.to_lowercase();
    if lower.contains("moving") || lower.contains("head") {
        Some(ModelKind::MovingHead)
    } else if lower.contains("par") || lower.contains("color") || lower.contains("dimmer") {
        Some(ModelKind::Par)
    } else if lower.contains("scanner") {
        Some(ModelKind::Scanner)
    } else if lower.contains("strobe") {
        Some(ModelKind::Strobe)
    } else if lower.contains("hazer") {
        Some(ModelKind::Hazer)
    } else if lower.contains("smoke") || lower.contains("fog") {
        Some(ModelKind::Smoke)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(kind: &str) -> Definition {
        Definition {
            kind: kind.into(),
            modes: Vec::new(),
            physical: None,
        }
    }

    /// No definition is bodyless. `Type` is free text a bundle author wrote,
    /// so a bar-ish name that is none of the exact strings still has to reach
    /// a body — and the fuzzy arm must not steal a par on the way.
    #[test]
    fn a_bar_by_any_name_is_procedural_and_nothing_else_is() {
        for kind in [
            "LED Bar (Pixels)",
            "LED Bar (Beams)",
            "LED Bar 12x30W RGBW",
            "Pixel Matrix",
        ] {
            assert!(is_procedural(&typed(kind)), "{kind} draws as a bar");
        }
        for kind in ["Color Changer", "Moving Head", "Par 64", "Laser"] {
            assert!(!is_procedural(&typed(kind)), "{kind} is not a bar");
        }
        // The two ways a fixture reaches a body, and every definition takes
        // one of them: a bar draws its box, everything else draws a mesh —
        // or, with no mesh kind, the box as well (`frame::housing_draws`).
        assert_eq!(model_kind(&typed("Par 64")), Some(ModelKind::Par));
        assert_eq!(model_kind(&typed("Laser")), None);
    }

    /// The reference point the whole concentration curve is anchored on.
    #[test]
    fn thirty_degree_spot_is_the_reference() {
        let cone = cone_from_opening(Luminaire {
            field_angle_deg: 30.0,
            lumens: 1.0,
        });
        assert!((cone.gain - 1.5).abs() < 1e-4);
        assert!((cone.range - 12.0).abs() < 1e-3);
        assert!((cone.cos_field - 15f32.to_radians().cos()).abs() < 1e-6);
    }
}
