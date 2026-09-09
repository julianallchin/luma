//! Venue environment, turned into light.
//!
//! [`VenueEnvironment`] is what a venue *is*; this module is the only place it
//! becomes something the renderer can draw. It answers in two halves because
//! the two halves are known at different times:
//!
//! - [`fill`] is the part that needs nothing but the environment — the clear
//!   colour, the ambient term and the one directional light. It is resolved
//!   when the render settings are constructed
//!   ([`RenderSettings::room`](crate::scene_desc::RenderSettings::room)).
//! - [`lamps`] and [`glow`] are the parts that need the *room* — where the
//!   house rig hangs, how far apart, and how far past the plan its light
//!   carries. They are resolved during frame assembly, which is the first
//!   moment the venue's bounds are known.
//!
//! House lighting is an overlapping grid of broad, warm downlights. The same
//! cones illuminate surfaces and scatter in haze. A small bounded ambient
//! term approximates unresolved room bounce; there is no directional work
//! light layered over the lamps. Object thumbnails keep their separate preset.

use glam::{Vec2, Vec3};
use luma_scene::Aabb;

use crate::frame::FixtureCone;
use crate::scene_desc::{DirectionalLight, Environment, SkyParams, VenueEnvironment};

/// Metres between house lamps. At the minimum six-metre mounting height,
/// neighbouring 60-degree beams overlap at their half-intensity shoulders.
pub const SPACING_M: f32 = 6.0;

/// Smallest room the grid will light, per side. An empty venue has no bounds
/// to speak of, and a house of one lamp is not a house.
pub const MIN_ROOM_M: f32 = 12.0;

/// Lowest the house rig ever hangs. A venue whose tallest piece is a metre of
/// deck still gets a ceiling.
pub const MIN_HEIGHT_M: f32 = 6.0;

/// How far above the tallest thing in the room the house rig hangs. "Above
/// rig height" is the whole rule: a house lamp inside the truss would light
/// the top of it and nothing else.
pub const CLEARANCE_M: f32 = 1.5;

/// Highest the house rig ever hangs, whatever is under it.
///
/// A venue can fly a truss at any trim, and a house that always cleared it
/// would leave the frame: the camera fits the *rig*, not the ceiling, so lamps
/// hung off the top of a tall room are lights nobody in the picture can see.
/// Twelve metres is a big room's ceiling and still inside a fitted front view.
pub const MAX_HEIGHT_M: f32 = 12.0;

/// Most lamps a house will ever have. The grid coarsens to fit rather than
/// refusing: a stadium gets wider spacing, not a frame-time cliff.
pub const MAX_LAMPS: usize = 64;

/// Roughly 3000 K, linear, pulled well toward neutral.
///
/// A true 3000 K white balanced against this renderer's D65 primaries is almost
/// amber, and measured on `Bill Graham Civic` a room lit by it came out brown:
/// every truss chord and deck face took the cast, and the picture read as a
/// sepia photograph rather than as a lit room. This is the same hue at a third
/// of that saturation — warm enough that a house pool is never mistaken for a
/// fixture in open white, neutral enough that aluminium under it still reads
/// as aluminium.
pub const WARM: Vec3 = Vec3::new(1.0, 0.80, 0.62);

/// Peak gain at full output, on the renderer's relative fixture scale.
/// Absolute photometry remains uncalibrated across the renderer.
const LAMP_GAIN: f32 = 0.6;

/// Broad auditorium downlight, based on the 60-degree ETC ArcSystem optic.
/// https://www.etcconnect.com/WorkArea/DownloadAsset.aspx?id=10737500333
/// The renderer's field angle is a zero-energy cutoff (not a measured 10%
/// field angle), so retain a soft skirt beyond the datasheet's 75.7° field.
const LAMP_FIELD_DEG: f32 = 90.0;
const LAMP_BEAM_DEG: f32 = 60.0;

/// Modest indirect fill while the renderer has no diffuse global illumination.
const BOUNCE_GAIN: f32 = 0.04;

/// Cull radius as a multiple of the hanging height, so a lamp's reach always
/// clears the floor with a pool's worth of spill around it.
const LAMP_REACH: f32 = 2.4;

/// One house lamp: where it hangs, and how hard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lamp {
    /// World position of the source.
    pub position: Vec3,
    /// Cone gain, already scaled by the house level.
    pub intensity: f32,
    /// Cull radius of this lamp's cone, metres.
    pub range: f32,
}

impl Lamp {
    /// This lamp as a light in the room, pointing straight down.
    ///
    /// An ordinary [`FixtureCone`]: the light index, the surface shader and the
    /// shadow-slot ranking all treat a house lamp exactly as they treat a par,
    /// because it is one. The same light also scatters through the haze.
    #[must_use]
    pub fn cone(self) -> FixtureCone {
        FixtureCone {
            position: self.position,
            range: self.range,
            direction: Vec3::NEG_Z,
            cos_beam: (LAMP_BEAM_DEG.to_radians() / 2.0).cos(),
            color: WARM,
            intensity: self.intensity,
            cos_field: (LAMP_FIELD_DEG.to_radians() / 2.0).cos(),
            // A downlight is the isotropic end of the model: no core, all
            // shoulder.
            wash: 1.0,
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
        }
    }
}

/// The bounds-free half of an environment: what to clear to, what to fill
/// with, and the one directional light.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    /// Clear colour and ambient term.
    pub environment: Environment,
    /// The single directional light, when the environment has one. Always
    /// `None` under a `sky`, which derives its own.
    pub sun: Option<DirectionalLight>,
    /// The atmosphere, when the room is open air.
    pub sky: Option<SkyParams>,
}

/// Resolve the bounds-free half of `env`.
///
/// # Indoor
///
/// A small ambient term approximates diffuse room bounce. Direct light comes
/// entirely from the downlights, so surfaces outside their reach no longer
/// receive an unrelated directional key. Bounce follows source output linearly;
/// changing a dimmer does not change the room's reflectance.
/// At zero the environment is [`Environment::DARK`].
///
/// # Outdoor
///
/// One line: the elevation becomes a [`SkyParams`], and the atmosphere supplies
/// the background, the ambient and the sun together. There is no ambient term
/// and no directional light of our own to blend with it — a sky that computed
/// its own sunlight next to a hand-authored key would be two suns.
///
/// This is the whole of the environment's outdoor half. Everything else about
/// an open-air venue — the dome, the scattering, the exposure — belongs to
/// [`SkyParams`] and `crate::atmosphere`.
#[must_use]
pub fn fill(env: VenueEnvironment) -> Fill {
    match env {
        VenueEnvironment::Indoor { .. } => {
            let level = env.house_level();
            Fill {
                environment: Environment {
                    background: Environment::DARK.background,
                    ambient_color: Environment::EDITOR.ambient_color,
                    ambient_intensity: BOUNCE_GAIN * level,
                    probe: None,
                },
                sun: None,
                sky: None,
            }
        }
        VenueEnvironment::Outdoor { .. } => Fill {
            // Under a sky none of this is read: the atmosphere is the
            // background and the ambient probe both. Left at DARK rather than
            // at a guess, so a frame that somehow loses its sky goes black
            // instead of quietly showing an invented sunset.
            environment: Environment::DARK,
            sun: None,
            sky: Some(SkyParams::outdoor(env.sun_elevation_deg())),
        },
    }
}

/// Hang a house rig over `room`.
///
/// The rule, entire: a lamp every [`SPACING_M`] metres across the room's
/// footprint — widened to [`MIN_ROOM_M`] a side if the venue is smaller than
/// that or empty — at [`CLEARANCE_M`] above the tallest thing in it, never
/// below [`MIN_HEIGHT_M`]. The grid is centred in the footprint and coarsened
/// until it fits [`MAX_LAMPS`], so the answer is bounded for any room.
///
/// Direct and indirect light both follow the dial linearly and reach zero
/// together at blackout.
///
/// Outdoors there is no house rig, so there are no lamps.
#[must_use]
pub fn lamps(env: VenueEnvironment, room: Aabb) -> Vec<Lamp> {
    let level = env.house_level();
    if !matches!(env, VenueEnvironment::Indoor { .. }) || level <= 0.0 {
        return Vec::new();
    }

    let (centre, size) = footprint(room);
    let height = (room.max.z + CLEARANCE_M).clamp(MIN_HEIGHT_M, MAX_HEIGHT_M);
    let (nx, ny) = counts(size);

    let intensity = LAMP_GAIN * level;
    // A dimmer this far down is a lamp the picture cannot resolve; the same
    // threshold every fixture cone is held to.
    if intensity < 0.01 {
        return Vec::new();
    }
    let range = height * LAMP_REACH;

    let mut out = Vec::with_capacity(nx * ny);
    for iy in 0..ny {
        for ix in 0..nx {
            out.push(Lamp {
                position: Vec3::new(
                    cell(centre.x, size.x, nx, ix),
                    cell(centre.y, size.y, ny, iy),
                    height,
                ),
                intensity,
                range,
            });
        }
    }
    out
}

/// The plan a house lights, and how far its light carries past it.
///
/// The ambient bounce approximation is confined to the room footprint with
/// a short edge fade. Direct lamp light already has angular and range falloff.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glow {
    /// Centre of the lit plan, world XY.
    pub centre: Vec2,
    /// Half-extents of the lit plan, world XY. Everything inside is lit as the
    /// house intends.
    pub half: Vec2,
    /// Metres beyond the plan over which the fill dies to nothing.
    ///
    /// A short fade avoids a hard edge where no wall geometry exists.
    pub margin: f32,
}

/// How far `env`'s fill reaches over `room`, or `None` when nothing bounds it.
///
/// `None` outdoors — an atmosphere lights the world because the world is what
/// it is lighting — and `None` with the house off, where there is no fill to
/// contain. Both cases leave every shaded point at full, which is also what a
/// scene with no house at all (a thumbnail, the dark stage) gets.
#[must_use]
pub fn glow(env: VenueEnvironment, room: Aabb) -> Option<Glow> {
    if !matches!(env, VenueEnvironment::Indoor { .. }) || env.house_level() <= 0.0 {
        return None;
    }
    let (centre, size) = footprint(room);
    let half = size.truncate() / 2.0;
    Some(Glow {
        centre: centre.truncate(),
        half,
        margin: SPACING_M / 2.0,
    })
}

/// Centre and size of the plan the grid covers, never smaller than
/// [`MIN_ROOM_M`] a side. An empty [`Aabb`] centres on the origin.
fn footprint(room: Aabb) -> (Vec3, Vec3) {
    let (centre, size) = if room.is_empty() {
        (Vec3::ZERO, Vec3::ZERO)
    } else {
        (room.center(), room.size())
    };
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    (
        Vec3::new(finite(centre.x), finite(centre.y), 0.0),
        Vec3::new(
            finite(size.x).max(MIN_ROOM_M),
            finite(size.y).max(MIN_ROOM_M),
            0.0,
        ),
    )
}

/// Lamps along each axis at [`SPACING_M`], coarsened together until the grid
/// fits [`MAX_LAMPS`]. Coarsened *together* so the grid stays square-ish: a
/// house that thinned only along its long axis would read as stripes.
fn counts(size: Vec3) -> (usize, usize) {
    let along = |extent: f32| ((extent / SPACING_M).round() as usize).max(1);
    let (mut nx, mut ny) = (along(size.x), along(size.y));
    while nx * ny > MAX_LAMPS {
        if nx >= ny {
            nx -= 1;
        } else {
            ny -= 1;
        }
    }
    (nx.max(1), ny.max(1))
}

/// Centre of cell `i` of `n` evenly dividing `extent` about `centre`.
fn cell(centre: f32, extent: f32, n: usize, i: usize) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let (n, i) = (n as f32, i as f32);
    centre - extent / 2.0 + extent * (i + 0.5) / n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(min: Vec3, max: Vec3) -> Aabb {
        Aabb::new(min, max)
    }

    #[test]
    fn house_fill_has_no_directional_work_light() {
        let env = VenueEnvironment::default();
        let fill = fill(env);
        assert!(fill.environment.ambient_intensity < 0.1);
        assert_eq!(fill.environment.background, [0.0; 3]);
        assert!(fill.sun.is_none());
        let room = room(Vec3::new(-9.0, -7.0, 0.0), Vec3::new(9.0, 7.0, 8.0));
        let glow = glow(env, room).expect("an indoor house is bounded by its room");
        assert_eq!(glow.half, Vec2::new(9.0, 7.0), "the whole room is at full");
    }

    #[test]
    fn house_bounce_has_a_short_edge_fade_even_in_large_rooms() {
        let wide = glow(
            VenueEnvironment::default(),
            room(Vec3::new(-20.0, -8.0, 0.0), Vec3::new(20.0, 8.0, 10.0)),
        )
        .expect("bounded");
        assert_eq!(wide.centre, Vec2::ZERO);
        assert_eq!(wide.margin, SPACING_M / 2.0);
        // ...and an empty venue's minimum room is a lit plan like any other.
        let empty = glow(VenueEnvironment::default(), Aabb::EMPTY).expect("bounded");
        assert_eq!(empty.half, Vec2::splat(MIN_ROOM_M / 2.0));
        assert_eq!(empty.margin, wide.margin);
    }

    /// An atmosphere lights the world because the world is what it is
    /// lighting, and a house at zero has no fill to contain.
    #[test]
    fn nothing_bounds_the_light_without_a_house() {
        assert!(glow(VenueEnvironment::outdoor(30.0), Aabb::EMPTY).is_none());
        assert!(glow(VenueEnvironment::indoor(0.0), Aabb::EMPTY).is_none());
    }

    #[test]
    fn an_indoor_house_at_zero_is_the_dark_stage() {
        let fill = fill(VenueEnvironment::indoor(0.0));
        assert_eq!(fill.environment, Environment::DARK);
        assert_eq!(fill.sun, None);
        assert!(lamps(VenueEnvironment::indoor(0.0), room(Vec3::ZERO, Vec3::ONE)).is_empty());
    }

    #[test]
    fn direct_and_indirect_light_follow_the_same_dimmer() {
        let half = VenueEnvironment::indoor(0.5);
        let ambient = fill(half).environment.ambient_intensity / BOUNCE_GAIN;
        let lamp = lamps(half, room(Vec3::ZERO, Vec3::ONE))[0].intensity / LAMP_GAIN;
        assert_eq!(ambient, 0.5);
        assert_eq!(lamp, ambient);
    }

    #[test]
    fn an_empty_venue_still_gets_a_room_sized_house() {
        let lamps = lamps(VenueEnvironment::default(), Aabb::EMPTY);
        assert!(!lamps.is_empty());
        for lamp in &lamps {
            assert!(lamp.position.z >= MIN_HEIGHT_M);
            assert!(lamp.position.x.abs() <= MIN_ROOM_M / 2.0);
            assert!(lamp.position.y.abs() <= MIN_ROOM_M / 2.0);
        }
    }

    #[test]
    fn the_rig_is_always_under_the_house() {
        let lamps = lamps(
            VenueEnvironment::default(),
            room(Vec3::new(-5.0, -5.0, 0.0), Vec3::new(5.0, 5.0, 9.0)),
        );
        for lamp in &lamps {
            assert!(lamp.position.z >= 9.0 + CLEARANCE_M);
        }
    }

    #[test]
    fn a_very_tall_room_keeps_its_house_in_frame() {
        let lamps = lamps(
            VenueEnvironment::default(),
            room(Vec3::new(-5.0, -5.0, 0.0), Vec3::new(5.0, 5.0, 30.0)),
        );
        for lamp in &lamps {
            assert!(lamp.position.z <= MAX_HEIGHT_M);
        }
    }

    #[test]
    fn a_stadium_coarsens_instead_of_exploding() {
        let lamps = lamps(
            VenueEnvironment::default(),
            room(Vec3::new(-90.0, -60.0, 0.0), Vec3::new(90.0, 60.0, 12.0)),
        );
        assert!(lamps.len() <= MAX_LAMPS, "{}", lamps.len());
        assert!(lamps.len() > MAX_LAMPS / 2, "{}", lamps.len());
    }

    #[test]
    fn a_house_lamp_scatters_into_the_haze() {
        for lamp in lamps(VenueEnvironment::default(), Aabb::EMPTY) {
            assert_eq!(lamp.cone().haze_gain, 1.0);
        }
    }

    #[test]
    fn neighbouring_house_beams_overlap_at_half_intensity() {
        let lamps = lamps(VenueEnvironment::default(), Aabb::EMPTY);
        let first = lamps[0].cone();
        let next = lamps[1].cone();
        let midpoint = (first.position + next.position) * 0.5;
        let floor = Vec3::new(midpoint.x, midpoint.y, 0.0);
        for cone in [first, next] {
            let ray = floor - cone.position;
            assert!(ray.normalize().dot(cone.direction) >= cone.cos_beam);
            assert!(ray.length() < cone.range);
        }
    }

    #[test]
    fn outdoors_has_no_house_rig() {
        assert!(lamps(VenueEnvironment::outdoor(45.0), Aabb::EMPTY).is_empty());
    }

    #[test]
    fn outdoors_the_atmosphere_is_the_only_light() {
        let fill = fill(VenueEnvironment::outdoor(37.0));
        assert_eq!(fill.sun, None, "a sky derives its own sun");
        assert_eq!(fill.environment.ambient_intensity, 0.0);
        assert_eq!(
            fill.sky.expect("open air has a sky").sun_elevation_deg,
            37.0
        );
    }

    #[test]
    fn indoors_there_is_no_sky() {
        assert!(fill(VenueEnvironment::default()).sky.is_none());
    }

    #[test]
    fn a_level_out_of_range_is_read_back_in_range() {
        assert_eq!(
            VenueEnvironment::Indoor { house_level: 9.0 }.house_level(),
            1.0
        );
        assert_eq!(
            VenueEnvironment::Indoor {
                house_level: f32::NAN
            }
            .house_level(),
            0.0
        );
    }
}
