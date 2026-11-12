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
//! House lighting is an overlapping grid of real auditorium downlights,
//! stated in lumens and converted by [`crate::photometry`], spaced so the
//! house at full lights the floor to about [`crate::photometry::METER_LUX`],
//! the level the fixed exposure is metered for. The same cones illuminate
//! surfaces and scatter in haze, and at their real output their haze is a
//! faint glow beside a moving head's beam. Their first bounce off the floor
//! is an ambient term (see [`fill`]); there is no directional work light
//! layered over the lamps. Object thumbnails keep their separate preset.
//!
//! Each lamp hangs in a body ([`PENDANT`], from the fixture library), lit or
//! dark, and a lit one's face is drawn at the lamp's own luminance: its
//! intensity toward the viewer over the face ([`Lamp::face_radiance`],
//! `Material::beam`). So a lamp glares at whoever stands in its beam and is
//! dark from beside it.

use glam::{Vec2, Vec3};
use luma_scene::Aabb;

use crate::frame::FixtureCone;
use crate::scene_desc::{DirectionalLight, Environment};
use luma_scene::environment::{SkyParams, VenueEnvironment};

/// Metres between house lamps. At the minimum six-metre mounting height,
/// neighbouring 60-degree beams overlap at their half-intensity shoulders,
/// and one [`LAMP_LUMENS`] lamp per six-metre square is [`FLOOR_LUX`].
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

/// One house lamp's output at full, lumens: an ETC ArcSystem Pro Four-Cell
/// Round, 3000 K, 60-degree optic — 7,334 field lumens, 9,880 cd peak, 84.2
/// degree field (ETC ArcSystem Pro Four-Cell Round datasheet, rev Q). A
/// pendant auditorium house light.
const LAMP_LUMENS: f32 = 7_334.0;

/// The same lamp's beam angle, from the datasheet. The renderer's field angle
/// is a zero-energy cutoff (not the measured 10% field angle), so it keeps a
/// soft skirt beyond the datasheet's 84.2°.
const LAMP_FIELD_DEG: f32 = 90.0;
const LAMP_BEAM_DEG: f32 = 60.0;

/// The lamp's emitting face, metres in radius: the face of the body it
/// hangs in ([`PENDANT`], 108 mm across). A face, not a point, so the light
/// close to it is spread over the face as the real one is; and the body's
/// own face, so the beam leaves through it and misses the lip around it.
const LAMP_RADIUS_M: f32 = 0.054;

/// Mean illuminance on the floor under the house at full, lux: one lamp's
/// lumens over the floor it serves. Edge cells lose some to spill.
pub const FLOOR_LUX: f32 = LAMP_LUMENS / (SPACING_M * SPACING_M);

/// The share of the floor's light a surface of any orientation sees: the
/// floor fills the lower half of the sky, a face looking down sees all of it,
/// a wall half and a face looking up none, and over all orientations that is
/// a half.
const FLOOR_VIEW: f32 = 0.5;

/// Cull radius as a multiple of the hanging height, so a lamp's reach always
/// clears the floor with a pool's worth of spill around it.
const LAMP_REACH: f32 = 2.4;

/// The body a house lamp hangs in: the fixture library's Elation Colour
/// Pendant, a house pendant of the ArcSystem's kind. Only its body: the light
/// stays the ArcSystem's above.
pub(crate) const PENDANT: &str = "elation/colour-pendant";

/// What a house lamp's meshes are keyed under in a frame.
pub(crate) const LAMP_KEY: &str = "::house-lamp";

/// Whether `mesh_key` names part of a house lamp's body.
pub(crate) fn is_lamp(mesh_key: &str) -> bool {
    mesh_key.starts_with(LAMP_KEY)
}

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
    /// The lamp's beam along `axis`, as a lit face has it (`Material::beam`).
    pub(crate) fn beam(axis: Vec3) -> luma_scene::mesh::Beam {
        luma_scene::mesh::Beam {
            axis,
            cos_beam: cos_half(LAMP_BEAM_DEG),
            cos_field: cos_half(LAMP_FIELD_DEG),
        }
    }

    /// What the lamp's face shows straight out, scene-linear: its intensity
    /// over the `area` (square metres) the light leaves from.
    pub(crate) fn face_radiance(self, area: f32) -> Vec3 {
        WARM * self.intensity * crate::photometry::BEAM_GAIN / area
    }

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
            cos_beam: cos_half(LAMP_BEAM_DEG),
            color: WARM,
            intensity: self.intensity,
            cos_field: cos_half(LAMP_FIELD_DEG),
            // A downlight is the isotropic end of the model: no core, all
            // shoulder.
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
            lens: luma_scene::luminaire::Lens {
                radius: LAMP_RADIUS_M,
            },
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
/// The ambient term is the house's first bounce off the floor: the floor,
/// lit to [`FLOOR_LUX`] times the level, reflects its albedo of that, and a
/// surface sees [`FLOOR_VIEW`] of the floor's light on average. The room has
/// no walls or ceiling to bounce from, so this is all of it. On a quality
/// level with reflection probes the probes see the lit floor themselves and
/// replace this term where they reach (`scene.wgsl`). It lights surfaces
/// only, never the haze. Direct light comes entirely from the downlights, so
/// surfaces outside their reach receive no unrelated directional key. Bounce
/// follows source output linearly; changing a dimmer does not change the
/// room's reflectance. At zero the environment is [`Environment::DARK`].
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
        VenueEnvironment::Indoor { .. } if env.house_level() <= 0.0 => Fill {
            environment: Environment::DARK,
            sun: None,
            sky: None,
        },
        VenueEnvironment::Indoor { .. } => {
            let level = env.house_level();
            let floor = crate::floor::mean_color(env.floor());
            Fill {
                environment: Environment {
                    background: Environment::DARK.background,
                    ambient_color: (white() * floor).to_array(),
                    ambient_intensity: FLOOR_VIEW
                        * FLOOR_LUX
                        * crate::photometry::SCENE_PER_NIT
                        * level,
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
            // The sky's ground is the venue's floor, run out to the horizon.
            sky: Some(SkyParams {
                clouds: env.clouds(),
                ground_albedo: crate::floor::mean_color(env.floor()).to_array(),
                ..SkyParams::outdoor(env.sun_elevation_deg(), env.sun_azimuth_deg())
            }),
        },
    }
}

/// Hang a house rig over `room`.
///
/// The rule, entire: a lamp every [`SPACING_M`] metres across the room's
/// footprint — widened to [`MIN_ROOM_M`] a side if the venue is smaller than
/// that or empty — at [`CLEARANCE_M`] above the tallest thing in it, never
/// below [`MIN_HEIGHT_M`]. The grid is centred in the footprint and coarsened
/// until it fits [`MAX_LAMPS`], so the answer is bounded for any room. Each
/// lamp is [`LAMP_LUMENS`] for a six-metre square of floor, scaled to the
/// floor it actually serves.
///
/// Direct and indirect light both follow the dial linearly and reach zero
/// together at blackout.
///
/// Outdoors there is no house rig, so there are no lamps.
#[must_use]
pub fn lamps(env: VenueEnvironment, room: Aabb) -> Vec<Lamp> {
    lamps_under(env, room, None)
}

/// How far under a ceiling the house rig hangs, in metres.
pub const CEILING_DROP_M: f32 = 0.5;

/// [`lamps`], in a room with a `ceiling` this many metres up: the rig hangs
/// just under it, however low it is, because a lamp above a ceiling lights
/// the top of the ceiling.
#[must_use]
pub fn lamps_under(env: VenueEnvironment, room: Aabb, ceiling: Option<f32>) -> Vec<Lamp> {
    let level = env.house_level();
    if !matches!(env, VenueEnvironment::Indoor { .. }) || level <= 0.0 {
        return Vec::new();
    }

    let (centre, size) = footprint(room);
    let height = match ceiling {
        Some(ceiling) => (ceiling - CEILING_DROP_M).max(CEILING_DROP_M),
        None => (room.max.z + CLEARANCE_M).clamp(MIN_HEIGHT_M, MAX_HEIGHT_M),
    };
    let (nx, ny) = counts(size);

    // A grid that had to spread out to fit the room or the lamp cap hangs a
    // bigger unit at each point, as a designer would: the floor stays at
    // FLOOR_LUX on average whatever the spacing.
    let cell_area = size.x / nx as f32 * (size.y / ny as f32);
    let lumens = LAMP_LUMENS * cell_area / (SPACING_M * SPACING_M);
    let intensity = level * cone_intensity(lumens);
    // A dimmer this far down is a lamp the picture cannot resolve.
    if level < 0.01 {
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

/// Where the house's lamps hang, lit or dark: their bodies stay up at
/// blackout. In the order [`lamps`] gives them.
#[must_use]
pub(crate) fn rig(env: VenueEnvironment, room: Aabb, ceiling: Option<f32>) -> Vec<Vec3> {
    if !matches!(env, VenueEnvironment::Indoor { .. }) {
        return Vec::new();
    }
    lamps_under(VenueEnvironment::indoor(1.0), room, ceiling)
        .into_iter()
        .map(|lamp| lamp.position)
        .collect()
}

/// The cone `intensity` that makes a [`WARM`] lamp of this profile emit
/// `lumens`: its colour is not unit luminance, so the flux is divided by it.
fn cone_intensity(lumens: f32) -> f32 {
    crate::photometry::cone_intensity(lumens, cos_half(LAMP_BEAM_DEG), cos_half(LAMP_FIELD_DEG))
        / luminance(WARM)
}

/// [`WARM`] at unit luminance: the colour of a lumen of house light.
fn white() -> Vec3 {
    WARM / luminance(WARM)
}

/// Rec. 709 luminance, the renderer's primaries.
fn luminance(c: Vec3) -> f32 {
    c.dot(Vec3::new(0.2126, 0.7152, 0.0722))
}

fn cos_half(full_deg: f32) -> f32 {
    (full_deg.to_radians() / 2.0).cos()
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

    /// Illuminance in lux on the floor at `point` from these lamps, as the
    /// surface pass computes it (`scene.wgsl`, unshadowed).
    fn floor_lux(lamps: &[Lamp], point: Vec3) -> f32 {
        lamps
            .iter()
            .map(|lamp| {
                let cone = lamp.cone();
                let q = point - cone.position;
                let d = q.length();
                let cos_angle = (q / d).dot(cone.direction);
                let profile = luma_scene::luminaire::angular_profile(
                    cos_angle,
                    cone.cos_beam,
                    cone.cos_field,
                );
                let taper = (1.0 - (d / cone.range).powi(4)).clamp(0.0, 1.0).powi(2);
                let cos_incidence = -(q / d).z;
                crate::photometry::BEAM_GAIN
                    * cone.intensity
                    * luminance(cone.color)
                    * profile
                    * taper
                    * cos_incidence
                    / (d * d)
            })
            .sum::<f32>()
            / crate::photometry::SCENE_PER_NIT
    }

    /// The house at full lights the floor where people stand to a
    /// house-lights-up level: within the band either side of EN 12464-1's
    /// 200 lx that a real room's uniformity allows, and near what the fixed
    /// exposure is metered for.
    #[test]
    fn a_full_house_lights_the_floor_like_house_lights() {
        let room = room(Vec3::new(-15.0, -12.0, 0.0), Vec3::new(15.0, 12.0, 5.0));
        let lamps = lamps(VenueEnvironment::default(), room);
        let mut sum = 0.0;
        let mut n = 0.0;
        for ix in -10..=10 {
            for iy in -8..=8 {
                // The interior, a lamp's spacing in from the walls.
                let point = Vec3::new(ix as f32, iy as f32, 0.0);
                let lux = floor_lux(&lamps, point);
                assert!((100.0..400.0).contains(&lux), "{lux} lx at {point}");
                sum += lux;
                n += 1.0;
            }
        }
        let mean = sum / n;
        assert!((150.0..300.0).contains(&mean), "{mean} lx");
        let metered = mean / crate::photometry::METER_LUX;
        assert!((0.67..1.5).contains(&metered), "{metered}");
    }

    /// Mean illuminance over the floor inside `room`, a metre inside its
    /// walls, lux.
    fn mean_floor_lux(room: Aabb) -> f32 {
        let lamps = lamps(VenueEnvironment::default(), room);
        let (mut sum, mut n) = (0.0, 0.0);
        let mut x = room.min.x + 1.0;
        while x < room.max.x - 1.0 {
            let mut y = room.min.y + 1.0;
            while y < room.max.y - 1.0 {
                sum += floor_lux(&lamps, Vec3::new(x, y, 0.0));
                n += 1.0;
                y += 1.0;
            }
            x += 1.0;
        }
        sum / n
    }

    /// A stadium's grid has to spread out to stay under the lamp cap; its
    /// lamps grow with it, so the floor is lit as well on average.
    #[test]
    fn the_floor_stays_lit_when_the_grid_coarsens() {
        let hall = mean_floor_lux(room(
            Vec3::new(-15.0, -12.0, 0.0),
            Vec3::new(15.0, 12.0, 10.0),
        ));
        let stadium = mean_floor_lux(room(
            Vec3::new(-90.0, -60.0, 0.0),
            Vec3::new(90.0, 60.0, 10.0),
        ));
        assert!((stadium / hall - 1.0).abs() < 0.25, "{hall} vs {stadium}");
    }

    /// A house lamp at its real output scatters a faint glow in haze, not a
    /// beam: its peak intensity is a small fraction of a stock moving head's,
    /// as an ETC ArcSystem's 9,880 cd is of a spot head's 100,000+.
    #[test]
    fn house_haze_is_faint_beside_a_moving_head() {
        let head = luma_scene::luminaire::cone_from_opening(luma_scene::luminaire::Luminaire {
            field_angle_deg: 36.0,
            lumens: 1.0,
        });
        let lamp = lamps(VenueEnvironment::default(), Aabb::EMPTY)[0].cone();
        let ratio = lamp.intensity * luminance(lamp.color) / head.gain;
        assert!(ratio < 0.15, "{ratio}");
    }

    /// The bounce is what the floor gives back: less than its albedo's share
    /// of the direct light, and darker over a darker floor.
    #[test]
    fn bounce_is_the_floors_reflection_of_the_house() {
        use luma_scene::environment::Floor;
        let bounce = |floor| {
            let env = VenueEnvironment::Indoor {
                house_level: 1.0,
                floor,
            };
            let e = fill(env).environment;
            luminance(Vec3::from(e.ambient_color)) * e.ambient_intensity
                / crate::photometry::SCENE_PER_NIT
        };
        let floors = Floor::options(true);
        let lux: Vec<f32> = floors.iter().map(|&f| bounce(f)).collect();
        for (floor, lux) in floors.iter().zip(&lux) {
            let albedo = luminance(crate::floor::mean_color(*floor));
            assert!(*lux > 0.0);
            assert!(*lux <= albedo * FLOOR_LUX * 1.001, "{floor:?}: {lux}");
        }
    }

    #[test]
    fn house_fill_has_no_directional_work_light() {
        let env = VenueEnvironment::default();
        let fill = fill(env);
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
        let room = room(Vec3::ZERO, Vec3::ONE);
        let (half, full) = (VenueEnvironment::indoor(0.5), VenueEnvironment::indoor(1.0));
        let ambient =
            fill(half).environment.ambient_intensity / fill(full).environment.ambient_intensity;
        let lamp = lamps(half, room)[0].intensity / lamps(full, room)[0].intensity;
        assert!((ambient - 0.5).abs() < 1e-6, "{ambient}");
        assert!((lamp - ambient).abs() < 1e-6, "{lamp}");
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

    /// Under a ceiling the rig hangs below it, in a low room and a tall one
    /// alike: the minimum and maximum heights are for rooms with no lid.
    #[test]
    fn a_ceiling_holds_the_house_under_it() {
        for ceiling in [3.0, 8.0, 20.0] {
            let lamps = lamps_under(
                VenueEnvironment::default(),
                room(Vec3::new(-5.0, -5.0, 0.0), Vec3::new(5.0, 5.0, ceiling)),
                Some(ceiling),
            );
            assert!(!lamps.is_empty());
            for lamp in &lamps {
                assert!(
                    lamp.position.z > 0.0 && lamp.position.z < ceiling,
                    "{ceiling} m ceiling, lamp at {}",
                    lamp.position.z
                );
            }
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
    fn outdoors_the_sky_takes_the_venue_sun_azimuth() {
        let sky = fill(VenueEnvironment::outdoor(20.0).with_sun_azimuth(135.0))
            .sky
            .expect("open air has a sky");
        assert_eq!(sky.sun_azimuth_deg, 135.0);
        let default = fill(VenueEnvironment::outdoor(20.0)).sky.expect("sky");
        assert_eq!(default.sun_azimuth_deg, SkyParams::DUSK.sun_azimuth_deg);
    }

    #[test]
    fn indoors_there_is_no_sky() {
        assert!(fill(VenueEnvironment::default()).sky.is_none());
    }

    #[test]
    fn a_level_out_of_range_is_read_back_in_range() {
        assert_eq!(
            VenueEnvironment::Indoor {
                house_level: 9.0,
                floor: luma_scene::environment::Floor::BlackStage,
            }
            .house_level(),
            1.0
        );
        assert_eq!(
            VenueEnvironment::Indoor {
                house_level: f32::NAN,
                floor: luma_scene::environment::Floor::BlackStage,
            }
            .house_level(),
            0.0
        );
    }
}
