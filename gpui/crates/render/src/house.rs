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
//! - [`lamps`] is the part that needs the *room* — where the house rig hangs
//!   and how far apart. It is resolved during frame assembly, which is the
//!   first moment the venue's bounds are known.
//!
//! # The editor work light is not a separate thing any more
//!
//! There used to be an `editor_lit` preset: an ambient term and a bright
//! directional key, switched on whenever a picture had to be legible. It was a
//! second lighting system living beside the venue's own, and nothing said how
//! the two composed. It is gone. What it used to produce is exactly
//! `Indoor { house_level: 1.0 }`, which is every venue's default — so the app
//! draws the picture it always drew, and now there is one answer to "why is
//! this room lit" instead of two.
//!
//! `RenderSettings::object_lit` survives for the *thumbnails*, whose subject is
//! one piece on nothing. That is not a room, and giving it a house would be
//! answering a question nobody asked.
//!
//! # House lights are physical
//!
//! Not an ambient lift: a sparse coarse grid of warm downlights hung above the
//! rig, each an ordinary [`FixtureCone`] going through the same light index
//! every beam in the room goes through, plus a small emissive disc so the
//! source itself is in the picture. A frame reads "the house is on" because
//! you can see the lamps and the pools they lay on the floor, which no scalar
//! multiply on albedo will ever say.
//!
//! They are excluded from the volumetric march ([`FixtureCone::haze_gain`]).
//! A house downlight is a diffuser, not a beam; sixty of them scattering into
//! the medium would fill the room with warm fog and cost a frame's whole budget
//! for a thing nobody wants to see.

use glam::Vec3;
use luma_scene::Aabb;

use crate::frame::FixtureCone;
use crate::scene_desc::{DirectionalLight, Environment, SkyParams, VenueEnvironment};

/// Metres between house lamps.
///
/// Sparse on purpose, and the number is tied to [`LAMP_FIELD_DEG`]: a lamp at
/// [`MAX_HEIGHT_M`] lays a pool a little under nine metres across, so at six
/// metres apart the pools touch and their centres are still the brightest
/// thing on the floor. At four they overlapped into a flat wash — sixty lights
/// spent saying what one ambient term already said.
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

/// How the lamps follow the dial.
///
/// Below one, so the sources hold on as the room goes: a house at 15% in a real
/// venue is a dim lamp you can still see and a pool you can still stand in, not
/// a lamp at a fifteenth of its output. [`fill`]'s square is the other end of
/// the same statement — see [`lamps`].
const LAMP_CURVE: f32 = 0.6;

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

/// One lamp's cone gain at `house_level = 1`.
///
/// Much larger than [`crate::luminaire::cone_from_opening`]'s scale would
/// suggest — a stock moving head at full is about 1.0 — and the reason is the
/// floor. The venue ground plane's albedo is `#030303`: it is a black room,
/// and a surface that reflects a thousandth of what lands on it needs a great
/// deal landing on it before a pool is a thing you can see. At 0.55 there was
/// no pool at any level, only a warm cast on the geometry. This is the level
/// at which a lamp six metres up lays a circle the eye finds.
const LAMP_GAIN: f32 = 3.5;

/// Full opening of a house downlight, degrees.
///
/// Wide for an optic and narrow for a diffuser, and the trade is the pool: at
/// 100 degrees a lamp ten metres up covers twenty-five metres of floor, every
/// lamp overlaps every neighbour, and the room comes out evenly grey. This is
/// the widest opening whose pool is smaller than [`SPACING_M`] is wide.
const LAMP_FIELD_DEG: f32 = 50.0;

/// Where the profile is still at half. A downlight's shoulder is soft; this is
/// what makes a pool a pool rather than a disc with an edge.
const LAMP_BEAM_DEG: f32 = 28.0;

/// Cull radius as a multiple of the hanging height, so a lamp's reach always
/// clears the floor with a pool's worth of spill around it.
const LAMP_REACH: f32 = 2.4;

/// Radius of the visible source, metres.
///
/// Generous for a lamp — the housing it stands for is smaller. A house rig is
/// looked at from across the room, where a true-sized disc is a pixel, and a
/// source you cannot see is not a source.
pub const LAMP_DISC_M: f32 = 0.20;

/// How bright a lamp's disc is drawn, as radiance.
///
/// The disc is a *source* seen directly, and a source is always far hotter
/// than what it lights — the display transform rolls its core toward white,
/// which is what makes a small bright thing read as a lamp rather than as a
/// warm coin. Scaled by the house level like everything else, so the sources
/// dim with the room instead of hanging there at full over a dark floor.
const DISC_GAIN: f32 = 30.0;

/// One house lamp: where it hangs, and how hard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lamp {
    /// World position of the source.
    pub position: Vec3,
    /// Linear RGB, already scaled by the house level.
    pub radiance: Vec3,
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
    /// because it is one. Only [`FixtureCone::haze_gain`] separates them.
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
            // The whole reason the field exists — see the module note.
            haze_gain: 0.0,
        }
    }

    /// Emissive radiance for the lamp's visible disc.
    #[must_use]
    pub fn emissive(self) -> Vec3 {
        self.radiance * DISC_GAIN
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
/// The fill is the room's own **bounce** — the light that has left the house
/// lamps, hit a wall and come back. This renderer has no global illumination,
/// so without it a downlight grid leaves every vertical face black, and a room
/// whose only lit surface is its floor is not a legible picture of a rig.
///
/// It is [`Environment::EDITOR`] and [`DirectionalLight::EDITOR`] scaled by the
/// square of the house level. At full it is *exactly* the pair the editor has
/// always used, so a default venue is the picture the app already drew. It
/// falls off faster than the lamps themselves because bounce does: halve the
/// source and you have halved the first bounce twice over. By the bottom of
/// the dial it is gone and the pools are the only thing left, which is what a
/// house at 10% looks like.
///
/// At zero this is [`Environment::DARK`] with no sun — the show-stage
/// environment, reached by turning the one dial down rather than by selecting
/// a second preset.
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
            let bounce = env.house_level() * env.house_level();
            Fill {
                environment: Environment {
                    background: scale(Environment::EDITOR.background, bounce),
                    ambient_color: Environment::EDITOR.ambient_color,
                    ambient_intensity: Environment::EDITOR.ambient_intensity * bounce,
                    probe: None,
                },
                sun: (bounce > 0.0).then(|| DirectionalLight {
                    intensity: DirectionalLight::EDITOR.intensity * bounce,
                    ..DirectionalLight::EDITOR
                }),
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
/// Intensity follows the level to the power [`LAMP_CURVE`], where [`fill`]
/// follows its square. That gap is the whole feel of the dial: the room goes
/// down much faster than the lamps do, so winding the house back leaves warm
/// circles on a dark floor rather than a uniformly grey picture. Both reach
/// zero together, so "house off" is off.
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

    let dial = level.powf(LAMP_CURVE);
    let radiance = WARM * dial;
    let intensity = LAMP_GAIN * dial;
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
                radiance,
                intensity,
                range,
            });
        }
    }
    out
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

fn scale(rgb: [f32; 3], by: f32) -> [f32; 3] {
    [rgb[0] * by, rgb[1] * by, rgb[2] * by]
}

/// The visible source: a flat disc of radius [`LAMP_DISC_M`] facing straight
/// down, at the origin of its own model space.
///
/// Emissive only — the lamp housing above it is not modelled, because from
/// every angle a stage camera ever takes it would be a black coin behind a
/// bright one. What has to be in the picture is the *source*: a room reads as
/// "the house is on" from the lamps being visibly lit, not from the pools
/// alone, which a low-angle camera may not even see.
#[must_use]
pub fn disc_mesh() -> crate::frame::MeshData {
    /// Enough segments that the rim reads as round at the size a lamp is ever
    /// drawn, and few enough that sixty of them are free.
    const SEGMENTS: usize = 16;

    let vertex = |x: f32, y: f32| crate::assets::Vertex {
        position: [x, y, 0.0],
        normal: [0.0, 0.0, -1.0],
        uv: [0.0, 0.0],
        tangent: [1.0, 0.0, 0.0, 1.0],
    };
    let mut vertices = vec![vertex(0.0, 0.0)];
    let mut indices = Vec::with_capacity(SEGMENTS * 3);
    for i in 0..SEGMENTS {
        #[allow(clippy::cast_precision_loss)]
        let angle = std::f32::consts::TAU * (i as f32) / (SEGMENTS as f32);
        vertices.push(vertex(LAMP_DISC_M * angle.cos(), LAMP_DISC_M * angle.sin()));
        // Wound so the face looks down, which is the way the normal points.
        let next = u32::try_from(i % SEGMENTS + 1).unwrap_or(1);
        let this = u32::try_from((i + 1) % SEGMENTS + 1).unwrap_or(1);
        indices.extend([0, this, next]);
    }
    crate::frame::MeshData {
        key: String::new(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(min: Vec3, max: Vec3) -> Aabb {
        Aabb::new(min, max)
    }

    #[test]
    fn an_indoor_house_at_full_is_the_editor_light_the_app_always_drew() {
        let fill = fill(VenueEnvironment::default());
        assert_eq!(fill.environment, Environment::EDITOR);
        assert_eq!(fill.sun, Some(DirectionalLight::EDITOR));
    }

    #[test]
    fn an_indoor_house_at_zero_is_the_dark_stage() {
        let fill = fill(VenueEnvironment::indoor(0.0));
        assert_eq!(fill.environment, Environment::DARK);
        assert_eq!(fill.sun, None);
        assert!(lamps(VenueEnvironment::indoor(0.0), room(Vec3::ZERO, Vec3::ONE)).is_empty());
    }

    #[test]
    fn the_room_darkens_faster_than_its_lamps_dim() {
        let half = VenueEnvironment::indoor(0.5);
        let ambient =
            fill(half).environment.ambient_intensity / Environment::EDITOR.ambient_intensity;
        let lamp = lamps(half, room(Vec3::ZERO, Vec3::ONE))[0].intensity / LAMP_GAIN;
        assert!(ambient < lamp, "{ambient} should be under {lamp}");
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
    fn a_house_lamp_never_scatters_into_the_haze() {
        for lamp in lamps(VenueEnvironment::default(), Aabb::EMPTY) {
            assert_eq!(lamp.cone().haze_gain, 0.0);
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
