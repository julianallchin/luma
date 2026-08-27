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
use glam::{Mat3, Vec3};

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

/// Openings outside this range are not physical and break the cone math.
fn clamp_field_angle(deg: f32) -> f32 {
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

/// The one answer to "how wide is this fixture's cone".
#[must_use]
pub fn luminaire_for(def: &Definition, kind: Option<ModelKind>) -> Luminaire {
    let (fallback_beam, lumens) = fallback(kind);
    let beam = lens_beam_angle(def).unwrap_or(fallback_beam);
    Luminaire {
        field_angle_deg: clamp_field_angle(beam * FIELD_PER_BEAM),
        lumens,
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
    let field_deg = clamp_field_angle(l.field_angle_deg);
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
/// `position` is `[pan, tilt]` in degrees, or `None` for a head with no pinned
/// state — the rest pose the mounting rotation alone describes. Pixel bars
/// have no pan or tilt: they fire along their mounted `+Z`, and passing one
/// here is not an error. A definition that is absent from the catalogue, or
/// one whose type emits haze rather than light, has no direction at all.
///
/// Composed in three space, because both the mounting Euler triple and the
/// pan/tilt gimbal are three-space conventions (`static-fixture.tsx`), then
/// taken to world space once at the end.
#[must_use]
pub fn beam_direction(def: Option<&Definition>, rot: [f32; 3], position: Option<[f32; 2]>) -> Vec3 {
    let Some(def) = def else {
        return Vec3::ZERO;
    };
    let local = if is_procedural(def) {
        Vec3::Z
    } else if model_kind(def).is_some_and(ModelKind::emits_beam) {
        let [pan, tilt] = position.unwrap_or([0.0, 0.0]);
        Mat3::from_rotation_y(pan.to_radians())
            * Mat3::from_rotation_x(-tilt.to_radians())
            * Vec3::NEG_Y
    } else {
        return Vec3::ZERO;
    };
    let mount = crate::coords::euler_xyz(rot[0], rot[2], rot[1]);
    (crate::coords::three_to_world_basis() * (mount * local)).normalize_or_zero()
}

/// LED bars and matrices are drawn from their layout rather than from a mesh.
#[must_use]
pub fn is_procedural(def: &Definition) -> bool {
    def.kind == "LED Bar (Pixels)" || def.kind == "LED Bar (Beams)"
}

/// Which bundled mesh a definition's `Type` selects. Port of
/// `getModelForFixture`, exact-match first and then the same fuzzy fallbacks.
#[must_use]
pub fn model_kind(def: &Definition) -> Option<ModelKind> {
    match def.kind.as_str() {
        "Color Changer" | "Dimmer" => return Some(ModelKind::Par),
        "Moving Head" => return Some(ModelKind::MovingHead),
        "Scanner" => return Some(ModelKind::Scanner),
        "Strobe" => return Some(ModelKind::Strobe),
        "Hazer" => return Some(ModelKind::Hazer),
        "Smoke" => return Some(ModelKind::Smoke),
        _ => {}
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
