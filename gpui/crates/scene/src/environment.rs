//! The room a venue is: indoor or open air, the sky and ground of an open-air
//! one, and the haze in either.
//!
//! These are **venue truth** — records on the venue row, synced with it — not
//! render dials. The renderer turns them into light (`luma_render::house`,
//! `luma_render::atmosphere`), and the backend stores, syncs and edits them,
//! which is why they live below the renderer.

use serde::{Deserialize, Deserializer, Serialize};

/// A physically based atmosphere: where the sun is, and nothing else.
///
/// The model is Hillaire (EGSR 2020) and lives in `luma_render::atmosphere`. Two
/// angles are the whole authored surface, because everything else about a sky
/// follows from them: the colour of the horizon, the colour of the sun, the
/// brightness of the day and the direction of every cast shadow are one
/// calculation, not four dials that can be set to disagree.
///
/// # Angles
///
/// Both are in the renderer's Z-up world. Elevation is degrees above the
/// horizon. Azimuth is degrees counter-clockwise from world +X, the same
/// convention the view fits in `crate::camera` use — so `+90` is toward
/// the crowd (+Y) and `270` is straight upstage (-Y).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkyParams {
    /// Degrees above the horizon. Negative is civil twilight, which the model
    /// renders rather than clamping away.
    pub sun_elevation_deg: f32,
    /// Degrees counter-clockwise from world +X.
    pub sun_azimuth_deg: f32,
    /// Albedo of the ground the sky bounces sunlight off, linear RGB, 0 to
    /// 1: the floor's mean colour. It is what fills the sky below the
    /// horizon, where a venue's floor dissolves into the background, and
    /// what the ambient light bounces up off the ground. A single number,
    /// as older descriptors hold, is read as grey.
    #[serde(deserialize_with = "albedo_rgb")]
    pub ground_albedo: [f32; 3],
    /// Display exposure, or `None` for the elevation-fitted default.
    ///
    /// The default is the honest answer for almost every frame: dusk to noon is
    /// many stops, and the fitted curve is what keeps a rig legible across all
    /// of them. Set it only to hold a deliberate look — a render that must
    /// match another at a different hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f32>,
    /// The cloud layer over the venue. Omitted when clear, so a descriptor
    /// written before clouds existed serializes unchanged.
    #[serde(default, skip_serializing_if = "CloudCover::is_clear")]
    pub clouds: CloudCover,
}

/// An RGB albedo, or a single number for a grey one.
pub fn albedo_rgb<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[f32; 3], D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Albedo {
        Grey(f32),
        Rgb([f32; 3]),
    }
    Ok(match Albedo::deserialize(deserializer)? {
        Albedo::Grey(value) => [value; 3],
        Albedo::Rgb(rgb) => rgb,
    })
}

impl SkyParams {
    /// Degrees off dead upstage the default sun stands.
    ///
    /// A sun on the centre line puts the disc behind the middle of the rig and
    /// the picture reads as a diagram. Twenty degrees is enough that the rake
    /// of the light is visible without losing the backlight.
    const DEFAULT_OFFSET_DEG: f32 = 20.0;

    /// The outdoor default: dusk behind the stage.
    ///
    /// Four degrees is the elevation where the ozone term is doing its most
    /// visible work — warm along the horizon, blue overhead — and where a rig
    /// backlit by the sun silhouettes. Upstage is world -Y, i.e. azimuth 270,
    /// offset so the disc is not dead centre.
    pub const DUSK: Self = Self {
        sun_elevation_deg: 4.0,
        sun_azimuth_deg: 270.0 - Self::DEFAULT_OFFSET_DEG,
        ground_albedo: [0.1; 3],
        exposure: None,
        clouds: CloudCover::Clear,
    };

    /// The sky for an open-air venue whose sun is `elevation_deg` up and
    /// `azimuth_deg` round.
    ///
    /// **This is the environment seam.** [`VenueEnvironment::Outdoor`] carries
    /// the sun's two angles, and this is the function that turns them into a
    /// sky: `luma_render::house::fill`'s outdoor arm returns it, and the atmosphere
    /// supplies the background, the ambient and the sun. The renderer prefers
    /// the sky's sun over an authored one whenever `luma_render::scene_desc::RenderSettings::sky` is
    /// set.
    #[must_use]
    pub fn outdoor(elevation_deg: f32, azimuth_deg: f32) -> Self {
        Self {
            sun_elevation_deg: if elevation_deg.is_finite() {
                elevation_deg.clamp(-90.0, 90.0)
            } else {
                Self::DUSK.sun_elevation_deg
            },
            sun_azimuth_deg: wrap_azimuth(azimuth_deg),
            ..Self::DUSK
        }
    }
}

/// An azimuth brought into 0..=360 degrees.
///
/// A value already in range is kept as it is, so 360 stays 360 and a slider
/// held at its top does not jump to its bottom. Any other finite value wraps
/// round the circle; a non-finite one reads as [`SkyParams::DUSK`]'s.
#[must_use]
pub fn wrap_azimuth(deg: f32) -> f32 {
    if !deg.is_finite() {
        SkyParams::DUSK.sun_azimuth_deg
    } else if (0.0..=360.0).contains(&deg) {
        deg
    } else {
        deg.rem_euclid(360.0)
    }
}

/// The cloud layer over an open-air venue: one of five skies.
///
/// A preset rather than dials, for the same reason [`SkyParams`] is two
/// angles: coverage, thickness, altitude and darkness are one weather, and
/// dials for each could be set to a sky nobody has seen. `luma_render::atmosphere`
/// turns each into a layer, and the same layer dims the sun, greys the
/// ambient and fills the background, so the three cannot disagree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloudCover {
    /// No clouds: the sky this app has always drawn.
    #[default]
    Clear,
    /// Small separate cumulus with blue sky between them.
    FairWeather,
    /// High, thin cirrus streaks: the sun shines through them.
    Wispy,
    /// Large cumulus over about half the sky; they can cover the sun.
    Scattered,
    /// One unbroken grey layer. The light is soft and has almost no shadow.
    Overcast,
    /// A thick, dark, low layer under a dim sky.
    Storm,
}

impl CloudCover {
    /// Every preset, clearest first: the order the editor offers them in.
    pub const ALL: [Self; 6] = [
        Self::Clear,
        Self::FairWeather,
        Self::Wispy,
        Self::Scattered,
        Self::Overcast,
        Self::Storm,
    ];

    /// Whether this is [`Self::Clear`]; serde's skip test.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        *self == Self::Clear
    }

    /// The name the editor shows, in sentence case.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Clear => "Clear",
            Self::FairWeather => "Fair weather",
            Self::Wispy => "Wispy",
            Self::Scattered => "Scattered",
            Self::Overcast => "Overcast",
            Self::Storm => "Storm",
        }
    }
}

/// The ground a venue stands on: one of a few real materials.
///
/// Venue truth, like [`CloudCover`]: it sits on the venue's environment
/// record. Each is a scanned or procedural CC0 material set
/// (`resources/meshes/floors`), drawn by `luma_render::floor`. An open-air venue
/// offers the ground kinds and a room offers floor coverings; concrete is
/// both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Floor {
    /// A lawn.
    Grass,
    /// Trodden earth with straw, patched with mud.
    Dirt,
    /// Fine, even sand.
    FineSand,
    /// A beach: pale dry sand with fine wind ripples.
    Beach,
    /// Coarse orange sand with grit.
    GravellySand,
    /// A car park or road.
    Asphalt,
    /// Worn concrete, indoors or out.
    Concrete,
    /// Fine grey gravel.
    Gravel,
    /// Dark, worn stage planks.
    StageDeck,
    /// Varnished hall floorboards.
    HallFloor,
    /// Matte black dance floor.
    BlackStage,
    /// Plain dark loop-pile carpet.
    Carpet,
}

impl Floor {
    /// The floors an open-air venue offers, in the editor's order.
    pub const OUTDOOR: [Self; 8] = [
        Self::Grass,
        Self::Dirt,
        Self::FineSand,
        Self::Beach,
        Self::GravellySand,
        Self::Asphalt,
        Self::Concrete,
        Self::Gravel,
    ];

    /// The floors a room offers, in the editor's order.
    pub const INDOOR: [Self; 5] = [
        Self::Concrete,
        Self::StageDeck,
        Self::HallFloor,
        Self::BlackStage,
        Self::Carpet,
    ];

    /// The floors a venue of this kind offers.
    #[must_use]
    pub fn options(indoor: bool) -> &'static [Self] {
        if indoor {
            &Self::INDOOR
        } else {
            &Self::OUTDOOR
        }
    }

    /// What a venue of this kind stands on until someone chooses: the
    /// nearest to the floor it had before floors existed. Indoors that was
    /// black paint; outdoors a plain grey ground.
    #[must_use]
    pub fn default_for(indoor: bool) -> Self {
        if indoor {
            Self::BlackStage
        } else {
            Self::Concrete
        }
    }

    fn is_indoor_default(&self) -> bool {
        *self == Self::default_for(true)
    }

    fn is_outdoor_default(&self) -> bool {
        *self == Self::default_for(false)
    }

    fn indoor_default() -> Self {
        Self::default_for(true)
    }

    fn outdoor_default() -> Self {
        Self::default_for(false)
    }

    /// The name the editor shows, in sentence case.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Grass => "Grass",
            Self::Dirt => "Dirt",
            Self::FineSand => "Fine sand",
            Self::Beach => "Beach",
            Self::GravellySand => "Gravelly sand",
            Self::Asphalt => "Asphalt",
            Self::Concrete => "Concrete",
            Self::Gravel => "Gravel",
            Self::StageDeck => "Stage deck",
            Self::HallFloor => "Hall floor",
            Self::BlackStage => "Black stage floor",
            Self::Carpet => "Carpet",
        }
    }

    /// The material set's directory under `resources/meshes/floors`, and its
    /// key in that directory's `floors.json`.
    #[must_use]
    pub fn set(self) -> &'static str {
        match self {
            Self::Grass => "grass",
            Self::Dirt => "dirt",
            Self::FineSand => "fine_sand",
            Self::Beach => "beach",
            Self::GravellySand => "gravelly_sand",
            Self::Asphalt => "asphalt",
            Self::Concrete => "concrete",
            Self::Gravel => "gravel",
            Self::StageDeck => "stage_deck",
            Self::HallFloor => "hall_floor",
            Self::BlackStage => "black_stage",
            Self::Carpet => "carpet",
        }
    }
}

/// What kind of room a venue is, and the one dial that mode has.
///
/// This is **venue truth**, not a render dial: it sits on the venue record
/// beside its name, and every picture of that room — the editor viewport, an
/// agent's offscreen frame — is taken under it. `luma_render::house` is the one
/// place it turns into light.
///
/// A room is either lit by its own house rig or by the sky. Indoors the one
/// question is "how far up?" — how bright the house is. Outdoors it is where
/// the sun is: how high, and from which side. Everything else about either
/// mode is derived, so there is nothing else to store and nothing that can
/// disagree.
///
/// The dials are read through [`Self::house_level`],
/// [`Self::sun_elevation_deg`] and [`Self::sun_azimuth_deg`], which clamp or
/// wrap: a value that arrived from a database column or an agent cannot put
/// the renderer in a state it has no answer for.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum VenueEnvironment {
    /// A room with a house rig over it. `house_level` is 0 (dark) to 1 (full).
    Indoor {
        /// How far up the house lights are.
        #[serde(rename = "houseLevel")]
        house_level: f32,
        /// The floor. A record written before floors existed, or one on
        /// the default, has no key.
        #[serde(
            default = "Floor::indoor_default",
            skip_serializing_if = "Floor::is_indoor_default"
        )]
        floor: Floor,
    },
    /// Open air. `sun_elevation_deg` is the time of day: -90 (midnight) to
    /// 90 (overhead).
    ///
    /// The sky and the sun light are not built here — see `luma_render::house`.
    Outdoor {
        /// Degrees above the horizon.
        #[serde(rename = "sunElevationDeg")]
        sun_elevation_deg: f32,
        /// Degrees counter-clockwise from world +X, as
        /// [`SkyParams::sun_azimuth_deg`]. A record written before this dial
        /// existed reads as [`SkyParams::DUSK`]'s.
        #[serde(rename = "sunAzimuthDeg", default = "dusk_azimuth")]
        sun_azimuth_deg: f32,
        /// The sky's cloud cover. A record written before clouds existed,
        /// or one that is clear, has no key: clear is the sky it always had.
        #[serde(default, skip_serializing_if = "CloudCover::is_clear")]
        clouds: CloudCover,
        /// The ground, as [`Self::Indoor`]'s floor.
        #[serde(
            default = "Floor::outdoor_default",
            skip_serializing_if = "Floor::is_outdoor_default"
        )]
        floor: Floor,
    },
}

impl Default for VenueEnvironment {
    /// Indoor, house at full: the picture this app has always drawn.
    fn default() -> Self {
        Self::indoor(1.0)
    }
}

impl VenueEnvironment {
    /// An indoor room at `level`, clamped to 0..=1.
    #[must_use]
    pub fn indoor(level: f32) -> Self {
        Self::Indoor {
            house_level: level.clamp(0.0, 1.0),
            floor: Floor::default_for(true),
        }
    }

    /// Open air with the sun `deg` above the horizon, clamped to -90..=90,
    /// at [`SkyParams::DUSK`]'s azimuth.
    #[must_use]
    pub fn outdoor(deg: f32) -> Self {
        Self::Outdoor {
            sun_elevation_deg: deg.clamp(-90.0, 90.0),
            sun_azimuth_deg: SkyParams::DUSK.sun_azimuth_deg,
            clouds: CloudCover::Clear,
            floor: Floor::default_for(false),
        }
    }

    /// The same room with the sun at azimuth `deg`, wrapped into 0..=360.
    /// Indoors there is no sun, so an indoor room is returned unchanged.
    #[must_use]
    pub fn with_sun_azimuth(self, deg: f32) -> Self {
        match self {
            Self::Indoor { .. } => self,
            Self::Outdoor {
                sun_elevation_deg,
                clouds,
                floor,
                ..
            } => Self::Outdoor {
                sun_elevation_deg,
                sun_azimuth_deg: wrap_azimuth(deg),
                clouds,
                floor,
            },
        }
    }

    /// The same room under `clouds`. Indoors there is no sky, so an indoor
    /// room is returned unchanged.
    #[must_use]
    pub fn with_clouds(self, clouds: CloudCover) -> Self {
        match self {
            Self::Indoor { .. } => self,
            Self::Outdoor {
                sun_elevation_deg,
                sun_azimuth_deg,
                floor,
                ..
            } => Self::Outdoor {
                sun_elevation_deg,
                sun_azimuth_deg,
                clouds,
                floor,
            },
        }
    }

    /// The same room on `floor`, or unchanged when this kind of venue does
    /// not offer it.
    #[must_use]
    pub fn with_floor(self, floor: Floor) -> Self {
        if !Floor::options(self.is_indoor()).contains(&floor) {
            return self;
        }
        match self {
            Self::Indoor { house_level, .. } => Self::Indoor { house_level, floor },
            Self::Outdoor {
                sun_elevation_deg,
                sun_azimuth_deg,
                clouds,
                ..
            } => Self::Outdoor {
                sun_elevation_deg,
                sun_azimuth_deg,
                clouds,
                floor,
            },
        }
    }

    /// The floor, always one this kind of venue offers: a record that names
    /// another reads as the default.
    #[must_use]
    pub fn floor(self) -> Floor {
        let floor = match self {
            Self::Indoor { floor, .. } | Self::Outdoor { floor, .. } => floor,
        };
        if Floor::options(self.is_indoor()).contains(&floor) {
            floor
        } else {
            Floor::default_for(self.is_indoor())
        }
    }

    fn is_indoor(self) -> bool {
        matches!(self, Self::Indoor { .. })
    }

    /// The sky's cloud cover, or [`CloudCover::Clear`] indoors.
    #[must_use]
    pub fn clouds(self) -> CloudCover {
        match self {
            Self::Indoor { .. } => CloudCover::Clear,
            Self::Outdoor { clouds, .. } => clouds,
        }
    }

    /// How far up the house is, or zero outdoors. Always in 0..=1.
    #[must_use]
    pub fn house_level(self) -> f32 {
        match self {
            Self::Indoor { house_level, .. } => {
                if house_level.is_finite() {
                    house_level.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            }
            Self::Outdoor { .. } => 0.0,
        }
    }

    /// The sun's elevation, or zero indoors. Always in -90..=90.
    #[must_use]
    pub fn sun_elevation_deg(self) -> f32 {
        match self {
            Self::Indoor { .. } => 0.0,
            Self::Outdoor {
                sun_elevation_deg: deg,
                ..
            } => {
                if deg.is_finite() {
                    deg.clamp(-90.0, 90.0)
                } else {
                    0.0
                }
            }
        }
    }

    /// The sun's azimuth, always in 0..=360. Indoors there is no sun, and
    /// this is [`SkyParams::DUSK`]'s.
    #[must_use]
    pub fn sun_azimuth_deg(self) -> f32 {
        match self {
            Self::Indoor { .. } => SkyParams::DUSK.sun_azimuth_deg,
            Self::Outdoor {
                sun_azimuth_deg, ..
            } => wrap_azimuth(sun_azimuth_deg),
        }
    }

    /// `"indoor"` or `"outdoor"` — the spelling the record, the agent verb and
    /// the editor's mode selector all use.
    #[must_use]
    pub fn mode(self) -> &'static str {
        match self {
            Self::Indoor { .. } => "indoor",
            Self::Outdoor { .. } => "outdoor",
        }
    }

    /// The JSON one venue record's `environment` column holds.
    ///
    /// The record spelling and the wire spelling are the same string on
    /// purpose: an agent reading `luma.venue.environment()`, the editor's mode
    /// selector and the database column are all looking at one value, and a
    /// second encoding would be a second place for them to disagree.
    #[must_use]
    pub fn to_record(self) -> String {
        serde_json::to_string(&self).unwrap_or_else(|_| String::from("{}"))
    }
}

fn dusk_azimuth() -> f32 {
    SkyParams::DUSK.sun_azimuth_deg
}

/// Read an environment back out of a venue record.
///
/// **Total.** A column that is empty, truncated, written by an older build or
/// simply wrong reads as the default — indoor, house at full — because there
/// is no useful thing for a venue to do with "this room has no lighting model"
/// and a failed read here would take the whole venue down with it.
impl From<String> for VenueEnvironment {
    fn from(record: String) -> Self {
        serde_json::from_str(&record).unwrap_or_default()
    }
}

impl From<VenueEnvironment> for String {
    fn from(environment: VenueEnvironment) -> Self {
        environment.to_record()
    }
}

/// How hazy a venue is, and what its haze looks like.
///
/// **Venue truth**, the same tier as [`VenueEnvironment`]: it sits on the venue
/// record, it syncs with the venue, and every picture of that room — the editor
/// viewport, an agent's offscreen frame — is taken through it. A room either
/// has atmosphere in it or it does not, and that is a property of the room, not
/// of whoever is looking at it.
///
/// Deliberately **without `steps` and `resolution`**. Those two live on
/// `luma_render::scene_desc::HazeSettings` and they are the march's cost, not the room's look: how
/// many samples a beam is integrated at, and at what fraction of the output
/// resolution. They belong to the machine drawing the frame — a laptop on
/// battery and a workstation should not have to disagree about what the venue
/// *is* in order to disagree about how expensive it may be — so they stay
/// local and unsynced. Resist adding them here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VenueHaze {
    /// Whether the participating-medium pass is evaluated for this room at all.
    pub enabled: bool,
    /// Mean density, independent of hazer fixtures. 0..=0.5.
    pub density: f32,
    /// Shared spatial appearance and movement.
    pub appearance: HazeAppearance,
}

impl Default for VenueHaze {
    /// The haze this app has always drawn: on, at 0.24, with the default
    /// appearance. A venue nobody has touched looks exactly as it did before
    /// haze became a venue property.
    fn default() -> Self {
        Self {
            enabled: true,
            density: 0.24,
            appearance: HazeAppearance::DEFAULT,
        }
    }
}

impl VenueHaze {
    /// Largest mean density a room may hold. Past this the march is fog rather
    /// than haze and a beam stops reading as a beam.
    pub const MAX_DENSITY: f32 = 0.5;

    /// Clamp authored or stored controls at the rendering boundary.
    ///
    /// Applied on the way out of [`From<String>`], so a row written by an older
    /// build, an agent or a hand-edited database cannot put the renderer in a
    /// state it has no answer for.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let default = Self::default();
        Self {
            enabled: self.enabled,
            density: if self.density.is_finite() {
                self.density.clamp(0.0, Self::MAX_DENSITY)
            } else {
                default.density
            },
            appearance: self.appearance.sanitized(),
        }
    }

    /// The JSON one venue record's `haze` column holds.
    ///
    /// The record spelling and the wire spelling are the same string, for the
    /// same reason as [`VenueEnvironment::to_record`]: the column, the agent
    /// verb and the editor's controls are all looking at one value, and a
    /// second encoding would be a second place for them to disagree.
    #[must_use]
    pub fn to_record(self) -> String {
        serde_json::to_string(&self).unwrap_or_else(|_| String::from("{}"))
    }
}

/// Read a venue's haze back out of its record.
///
/// **Total**, and sanitized. A column that is empty, truncated, written by an
/// older build or simply wrong reads as the default, because there is no useful
/// thing for a venue to do with "this room has no atmosphere model" and a
/// failed read here would take the whole venue down with it.
impl From<String> for VenueHaze {
    fn from(record: String) -> Self {
        serde_json::from_str::<Self>(&record)
            .unwrap_or_default()
            .sanitized()
    }
}

impl From<VenueHaze> for String {
    fn from(haze: VenueHaze) -> Self {
        haze.to_record()
    }
}

/// Appearance of a shared procedural medium, independent of fixture output.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HazeAppearance {
    /// Density contrast: zero is uniform, one has pronounced clouds.
    pub cloudiness: f32,
    /// The largest eddy, metres: the clouds' size.
    pub cloud_size: f32,
    /// How hard the air is stirred: zero is still, one a fan on the haze.
    /// Eddies of every size move and turn over in proportion.
    pub turbulence: f32,
    /// Horizontal drift in metres per second.
    pub wind_speed: f32,
    /// World XY direction, degrees from +X toward +Y.
    pub wind_direction: f32,
}

impl Default for HazeAppearance {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl HazeAppearance {
    /// Subtle indoor haze with a gentle horizontal drift.
    pub const DEFAULT: Self = Self {
        cloudiness: 0.35,
        cloud_size: 4.0,
        turbulence: 0.3,
        wind_speed: 0.15,
        wind_direction: 30.0,
    };
    /// Clamp invalid authored controls at the rendering boundary.
    pub fn sanitized(self) -> Self {
        let default = Self::default();
        let bounded = |v: f32, fallback: f32, min: f32, max: f32| {
            if v.is_finite() {
                v.clamp(min, max)
            } else {
                fallback
            }
        };
        Self {
            cloudiness: bounded(self.cloudiness, default.cloudiness, 0.0, 1.0),
            cloud_size: bounded(self.cloud_size, default.cloud_size, 0.5, 20.0),
            turbulence: bounded(self.turbulence, default.turbulence, 0.0, 1.0),
            wind_speed: bounded(self.wind_speed, default.wind_speed, 0.0, 10.0),
            wind_direction: if self.wind_direction.is_finite() {
                self.wind_direction.rem_euclid(360.0)
            } else {
                default.wind_direction
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_azimuth_round_trips_through_the_environment_record() {
        let environment = VenueEnvironment::outdoor(12.0).with_sun_azimuth(95.0);
        assert_eq!(VenueEnvironment::from(environment.to_record()), environment);
        assert_eq!(environment.sun_azimuth_deg(), 95.0);
        assert_eq!(environment.sun_elevation_deg(), 12.0);
    }

    #[test]
    fn clouds_round_trip_and_a_clear_sky_writes_no_key() {
        let clear = VenueEnvironment::outdoor(12.0).with_sun_azimuth(95.0);
        assert!(
            !clear.to_record().contains("clouds"),
            "{}",
            clear.to_record()
        );
        for clouds in CloudCover::ALL {
            let environment = clear.with_clouds(clouds);
            assert_eq!(VenueEnvironment::from(environment.to_record()), environment);
            assert_eq!(environment.clouds(), clouds);
        }
        // Changing the sun keeps the weather.
        let storm = clear.with_clouds(CloudCover::Storm);
        assert_eq!(storm.with_sun_azimuth(10.0).clouds(), CloudCover::Storm);
        // Indoors there is no sky to cloud.
        let indoor = VenueEnvironment::indoor(0.5);
        assert_eq!(indoor.with_clouds(CloudCover::Storm), indoor);
        assert_eq!(indoor.clouds(), CloudCover::Clear);
    }

    #[test]
    fn floors_round_trip_per_kind_of_venue_and_the_default_writes_no_key() {
        for indoor in [true, false] {
            let room = if indoor {
                VenueEnvironment::indoor(0.5)
            } else {
                VenueEnvironment::outdoor(12.0).with_clouds(CloudCover::Storm)
            };
            assert_eq!(room.floor(), Floor::default_for(indoor));
            assert!(!room.to_record().contains("floor"), "{}", room.to_record());
            for &floor in Floor::options(indoor) {
                let chosen = room.with_floor(floor);
                assert_eq!(VenueEnvironment::from(chosen.to_record()), chosen);
                assert_eq!(chosen.floor(), floor);
            }
        }
        // A room offers no grass, and an open-air venue no carpet.
        let indoor = VenueEnvironment::indoor(0.5);
        assert_eq!(indoor.with_floor(Floor::Grass), indoor);
        let outdoor = VenueEnvironment::outdoor(12.0).with_floor(Floor::Gravel);
        assert_eq!(outdoor.with_floor(Floor::Carpet), outdoor);
        // Moving the sun and changing the weather keep the ground.
        let moved = outdoor
            .with_sun_azimuth(10.0)
            .with_clouds(CloudCover::Storm);
        assert_eq!(moved.floor(), Floor::Gravel);
        // Records from before floors existed read as the default.
        let old = VenueEnvironment::from(String::from(r#"{"mode":"indoor","houseLevel":0.3}"#));
        assert_eq!(old.floor(), Floor::BlackStage);
    }

    #[test]
    fn a_record_without_an_azimuth_reads_the_dusk_default() {
        let old = String::from(r#"{"mode":"outdoor","sunElevationDeg":20.0}"#);
        let environment = VenueEnvironment::from(old);
        assert_eq!(environment.sun_elevation_deg(), 20.0);
        assert_eq!(
            environment.sun_azimuth_deg(),
            SkyParams::DUSK.sun_azimuth_deg
        );
    }

    #[test]
    fn sun_azimuth_wraps_into_a_full_turn() {
        let at = |deg| {
            VenueEnvironment::outdoor(10.0)
                .with_sun_azimuth(deg)
                .sun_azimuth_deg()
        };
        assert_eq!(at(0.0), 0.0);
        assert_eq!(at(360.0), 360.0, "the top of the slider stays at the top");
        assert_eq!(at(370.0), 10.0);
        assert_eq!(at(-30.0), 330.0);
        assert_eq!(at(f32::NAN), SkyParams::DUSK.sun_azimuth_deg);
        // Stored past the range by another writer, read back in range.
        let stored = VenueEnvironment::Outdoor {
            sun_elevation_deg: 10.0,
            sun_azimuth_deg: 725.0,
            clouds: CloudCover::Clear,
            floor: Floor::Concrete,
        };
        assert_eq!(stored.sun_azimuth_deg(), 5.0);
        // Indoors there is no sun to turn.
        let indoor = VenueEnvironment::indoor(0.5);
        assert_eq!(indoor.with_sun_azimuth(10.0), indoor);
    }

    #[test]
    fn venue_haze_round_trips_through_its_record() {
        let haze = VenueHaze {
            enabled: false,
            density: 0.42,
            appearance: HazeAppearance {
                cloudiness: 0.6,
                cloud_size: 7.5,
                turbulence: 0.2,
                wind_speed: 1.25,
                wind_direction: 210.0,
            },
        };
        assert_eq!(VenueHaze::from(haze.to_record()), haze);
    }

    #[test]
    fn a_malformed_haze_record_reads_as_the_default() {
        for record in ["", "{", "null", "[]", "not json at all"] {
            assert_eq!(
                VenueHaze::from(record.to_string()),
                VenueHaze::default(),
                "{record:?} should have decoded to the default"
            );
        }
    }

    #[test]
    fn an_out_of_range_haze_record_is_clamped_on_the_way_in() {
        let record = r#"{"enabled":true,"density":9.0,"appearance":{"cloudiness":-1.0,"cloudSize":900.0,"turbulence":5.0,"windSpeed":-3.0,"windDirection":400.0}}"#;
        let haze = VenueHaze::from(record.to_string());
        assert_eq!(haze.density, VenueHaze::MAX_DENSITY);
        assert_eq!(haze.appearance, haze.appearance.sanitized());
    }
}
