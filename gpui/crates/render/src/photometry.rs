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
//!   constant `K` = 12.5, flat-disc incident constant `C` = 250). 0 EV is the
//!   reading for [`METER_LUX`], so at 0 EV one candela per square metre is
//!   [`SCENE_PER_NIT`] scene-linear units. A camera that meters (an eye, too)
//!   adapts away from it, but only over [`ADAPT_EV`], a range stated in lux:
//!   a dim room stays dim and a blinding one stays blinding.
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
pub(crate) const MIDDLE_GREY: f32 = 0.18;

/// Scene-linear units per candela per square metre at the fixed exposure.
///
/// The surface a meter puts on middle grey under [`METER_LUX`] has luminance
/// `K/C · METER_LUX` (10 cd/m² here); that luminance is scene-linear
/// [`MIDDLE_GREY`].
pub const SCENE_PER_NIT: f32 = MIDDLE_GREY * METER_C / (METER_K * METER_LUX);

/// The exposure, in stops from 0 EV, that an incident meter reading `lux`
/// gives: positive opens up for a darker room.
#[must_use]
pub fn ev_for_lux(lux: f32) -> f32 {
    (METER_LUX / lux).log2()
}

/// The stops metering may move the exposure through, in incident-meter
/// readings ([`ev_for_lux`]):
///
/// - **−6 EV, 12,800 lx**, the brightest light it adapts to: a stage lit for
///   broadcast (1,000 to 2,000 lx of key) with the beams of a full show on top
///   of it, and short of daylight.
/// - **+4 EV, 12.5 lx**, the darkest: a dark show, lit by nothing but its
///   fixtures. A concert video camera wide open — ISO 6400, f/2.8, 1/50 s,
///   EV100 2.6 — is correctly exposed for an incident reading of about 12 lx
///   (EV100 = log2(E·100/C)). Below it a dim cue is meant to look dim, not be
///   lifted to grey.
///
/// The 2^n lux values keep the ends whole stops.
pub const ADAPT_EV: std::ops::RangeInclusive<f32> = -6.0..=4.0;

/// The absolute scale of every cone, surface and haze alike: scene-linear
/// irradiance at one metre on axis per unit of cone `intensity`.
///
/// A cone's `intensity` is a dimmer times its optic's gain, the peak
/// intensity in relative lumens per steradian
/// (`luma_scene::luminaire::cone_from_opening`), so one unit is
/// [`luma_scene::luminaire::LUMENS_PER_UNIT`] candela, and this is that many
/// candela at [`SCENE_PER_NIT`].
pub const BEAM_GAIN: f32 = SCENE_PER_NIT * luma_scene::luminaire::LUMENS_PER_UNIT;

/// The `intensity` a cone with this profile needs to emit `lumens` real
/// lumens of white, with [`BEAM_GAIN`] applied after.
#[must_use]
pub fn cone_intensity(lumens: f32, cos_beam: f32, cos_field: f32) -> f32 {
    lumens
        / luma_scene::luminaire::LUMENS_PER_UNIT
        / luma_scene::luminaire::profile_solid_angle(cos_beam, cos_field)
}

/// Scene-linear radiance per unit of a laser projector's light, per metre of
/// beam and unit scattering, as `laser.wgsl` reads it (`laser.rs`'s `GAIN`).
///
/// One unit is a full-white beam of [`LASER_LUMENS`]. A haze cone scatters
/// `σ` times its phase (relative to isotropic) per lumen-per-square-metre
/// crossing it, at [`SCENE_PER_NIT`]; a laser beam is the same light in a
/// line, so its gain is the same scale times its lumens.
pub const LASER_GAIN: f32 = SCENE_PER_NIT * LASER_LUMENS;

/// A club RGB laser projector's white, lumens: about 3 W of red, green and
/// blue diodes (a mid-size show laser) at about 200 lm/W for a white mix of
/// 638, 520 and 450 nm (683 lm/W × V(λ), weighted by the power each needs
/// for white: green carries most of it at about 480 lm/W).
const LASER_LUMENS: f32 = 600.0;

#[cfg(test)]
mod tests {
    use super::*;
    use luma_scene::luminaire::{angular_profile, cone_from_opening, Luminaire, LUMENS_PER_UNIT};

    /// The profile's solid angle by brute force.
    fn dense_solid_angle(cos_beam: f32, cos_field: f32) -> f32 {
        const STEPS: usize = 20_000;
        let step = (1.0 - cos_field) / STEPS as f32;
        let sum: f32 = (0..STEPS)
            .map(|i| angular_profile(cos_field + (i as f32 + 0.5) * step, cos_beam, cos_field))
            .sum();
        std::f32::consts::TAU * sum * step
    }

    /// Every opening, a pencil beam to a flood, emits exactly its lumens at
    /// the renderer's scale.
    #[test]
    fn every_opening_emits_its_lumens() {
        for field_angle_deg in [4.0, 8.0, 36.0, 60.0, 120.0, 160.0] {
            let cone = cone_from_opening(Luminaire {
                field_angle_deg,
                lumens: 1.0,
            });
            let flux = BEAM_GAIN * cone.gain * dense_solid_angle(cone.cos_beam, cone.cos_field);
            let expected = SCENE_PER_NIT * LUMENS_PER_UNIT;
            assert!(
                (flux / expected - 1.0).abs() < 0.005,
                "{field_angle_deg}°: {flux} vs {expected}"
            );
        }
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
    fn adaptation_spans_a_dark_show_to_a_broadcast_stage() {
        const BRIGHTEST_LUX: f32 = 12_800.0;
        const DARKEST_LUX: f32 = 12.5;
        assert_eq!(*ADAPT_EV.start(), ev_for_lux(BRIGHTEST_LUX));
        assert_eq!(*ADAPT_EV.end(), ev_for_lux(DARKEST_LUX));
        assert!(ADAPT_EV.contains(&ev_for_lux(METER_LUX)));
    }

    #[test]
    fn the_same_lumens_through_a_wider_cone_is_less_intense() {
        let narrow = cone_intensity(1000.0, 10f32.to_radians().cos(), 20f32.to_radians().cos());
        let wide = cone_intensity(1000.0, 30f32.to_radians().cos(), 45f32.to_radians().cos());
        assert!(wide < narrow);
    }
}
