//! Real light, in the renderer's units.
//!
//! The renderer works in scene-linear values, and the tone curve takes
//! scene-linear 0.18 to middle grey. This module fixes what one of those
//! values *is*: a luminance in candela per square metre, seen by a camera
//! exposed for a room lit to [`METER_LUX`]. Every light is then stated in
//! real units and converted here:
//!
//! - **Exposure.** An incident meter reading `E` lux exposes so that a surface
//!   of luminance `K/C · E` lands on middle grey (ISO 2720: reflected-meter
//!   constant `K` = 12.5, flat-disc incident constant `C` = 250). The camera is
//!   fixed at the reading for [`METER_LUX`], so one candela per square metre
//!   is [`SCENE_PER_NIT`] scene-linear units, whatever is on stage. There is no
//!   metering in it: a dimmer that goes down makes the picture darker.
//! - **Fixtures.** A cone's `intensity` is a dimmer times its optic's gain
//!   (`luma_scene::luminaire::cone_from_opening`), on a scale where one unit
//!   of relative lumens is [`luma_scene::luminaire::LUMENS_PER_UNIT`] real
//!   lumens. [`BEAM_GAIN`] is the factor that makes a stock moving head's cone
//!   carry exactly that flux at this exposure.
//! - **Anything stated in lumens** (the house lamps) takes its cone intensity
//!   from [`cone_intensity`], so its flux is right on the same scale.
//!
//! A surface lit to `E` lux with albedo `ρ` has luminance `ρE/π`; the
//! renderer shades `ρ/π` times its irradiance, so irradiance in the renderer is
//! also [`SCENE_PER_NIT`] per lux.

/// The illuminance the fixed exposure is metered for, lux: a room with the
/// house lights up.
///
/// EN 12464-1:2011, theatres, concert halls and cinemas, seating areas for
/// maintenance and cleaning: 200 lx maintained. That is the "house
/// lights up" level — enough to find a seat and see the room. A performance
/// house dims well below it.
pub const METER_LUX: f32 = 200.0;

/// Reflected-light meter calibration constant, cd·s/m² (ISO 2720).
const METER_K: f32 = 12.5;
/// Incident-light meter calibration constant for a flat receptor, lx·s
/// (ISO 2720).
const METER_C: f32 = 250.0;
/// Scene-linear middle grey, which the tone curves take to their mid-tone.
const MIDDLE_GREY: f32 = 0.18;

/// Scene-linear units per candela per square metre at the fixed exposure.
///
/// The surface a meter puts on middle grey under [`METER_LUX`] has luminance
/// `K/C · METER_LUX` (10 cd/m² here); that luminance is scene-linear
/// [`MIDDLE_GREY`].
pub const SCENE_PER_NIT: f32 = MIDDLE_GREY * METER_C / (METER_K * METER_LUX);

/// The absolute scale of every cone, surface and haze alike: scene-linear
/// irradiance at one metre on axis per unit of cone `intensity`.
///
/// Chosen so that a stock moving head (18 degree beam, one relative lumen;
/// `luma_scene::luminaire`) emits [`luma_scene::luminaire::LUMENS_PER_UNIT`]
/// real lumens at [`SCENE_PER_NIT`]. Pinned by
/// `tests::beam_gain_is_a_stock_head_in_real_lumens`.
pub const BEAM_GAIN: f32 = 1_455.6;

/// Solid angle of a cone's angular profile, steradians: the integral of
/// `fixture_light.wgsl`'s `angular_profile` over the sphere. A cone whose
/// peak intensity is `I` emits `I` times this.
#[must_use]
pub fn profile_solid_angle(cos_beam: f32, cos_field: f32) -> f32 {
    const STEPS: usize = 2048;
    let span = 1.0 - cos_field;
    let step = span / STEPS as f32;
    let sum: f32 = (0..STEPS)
        .map(|i| angular_profile(cos_field + (i as f32 + 0.5) * step, cos_beam, cos_field))
        .sum();
    std::f32::consts::TAU * sum * step
}

/// The `intensity` a cone with this profile needs to emit `lumens` real
/// lumens of white at the fixed exposure, with [`BEAM_GAIN`] applied after.
#[must_use]
pub fn cone_intensity(lumens: f32, cos_beam: f32, cos_field: f32) -> f32 {
    SCENE_PER_NIT * lumens / (BEAM_GAIN * profile_solid_angle(cos_beam, cos_field))
}

/// `fixture_light.wgsl`'s `angular_profile`, term for term: Gaussian in angle
/// with its half point at the beam edge, faded to zero at the field edge.
#[must_use]
pub(crate) fn angular_profile(cos_angle: f32, cos_beam: f32, cos_field: f32) -> f32 {
    if cos_angle <= cos_field {
        return 0.0;
    }
    let t = (1.0 - cos_angle) / (1.0 - cos_beam).max(1e-5);
    let peak = (-std::f32::consts::LN_2 * t).exp();
    let edge = cos_field + (cos_beam - cos_field) * 0.35;
    let x = ((cos_angle - cos_field) / (edge - cos_field)).clamp(0.0, 1.0);
    peak * x * x * (3.0 - 2.0 * x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use luma_scene::luminaire::{cone_from_opening, Luminaire, LUMENS_PER_UNIT};

    #[test]
    fn beam_gain_is_a_stock_head_in_real_lumens() {
        // The fallback moving head: an 18 degree beam, a field twice that.
        let cone = cone_from_opening(Luminaire {
            field_angle_deg: 36.0,
            lumens: 1.0,
        });
        let flux = BEAM_GAIN * cone.gain * profile_solid_angle(cone.cos_beam, cone.cos_field);
        let expected = SCENE_PER_NIT * LUMENS_PER_UNIT;
        assert!(
            (flux / expected - 1.0).abs() < 0.005,
            "{flux} vs {expected}"
        );
    }

    #[test]
    fn a_grey_card_under_the_metered_light_is_middle_grey() {
        // An incident meter is calibrated so a card of reflectance π·K/C
        // (about 16%) under the light it reads comes out middle grey.
        let card = std::f32::consts::PI * METER_K / METER_C;
        let luminance = card * METER_LUX / std::f32::consts::PI;
        assert!((luminance * SCENE_PER_NIT - MIDDLE_GREY).abs() < 1e-6);
    }

    #[test]
    fn a_wider_profile_holds_more_light() {
        let narrow = profile_solid_angle(10f32.to_radians().cos(), 20f32.to_radians().cos());
        let wide = profile_solid_angle(30f32.to_radians().cos(), 45f32.to_radians().cos());
        assert!(wide > narrow);
        // ...and never more than the cap of the cone it lives in.
        let cap = std::f32::consts::TAU * (1.0 - 45f32.to_radians().cos());
        assert!(wide < cap);
    }

    #[test]
    fn the_same_lumens_through_a_wider_cone_is_less_intense() {
        let narrow = cone_intensity(1000.0, 10f32.to_radians().cos(), 20f32.to_radians().cos());
        let wide = cone_intensity(1000.0, 30f32.to_radians().cos(), 45f32.to_radians().cos());
        assert!(wide < narrow);
    }
}
