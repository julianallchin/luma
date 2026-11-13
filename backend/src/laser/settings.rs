//! What a laser projector is: its diodes, its scanner, its beam, and how
//! its output is trimmed to the box on the truss.
//!
//! Every projector is a box with an aperture and a scanner, so there is no
//! library of models: a projector is its [`Settings`], and a [`Preset`] is a
//! place to start. They are stored as the projector's venue node parameters,
//! one number per [`Key`]; a key the node does not hold reads as the 3 W
//! preset's.

use glam::{DMat3, DVec3};
use luma_scene::spectrum::{observer, PEAK_LUMENS_PER_WATT, SRGB_TO_XYZ, XYZ_TO_SRGB};
use luma_scene::venue::Params;

/// One setting of a projector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Optical power of the red, green and blue diodes at the aperture, watts.
    PowerRed,
    PowerGreen,
    PowerBlue,
    /// Their wavelengths, nanometres.
    WavelengthRed,
    WavelengthGreen,
    WavelengthBlue,
    /// The widest the scanner opens, across and up, optical degrees.
    Angle,
    /// The scanner's rating: thousands of points a second at ILDA's 8° test
    /// pattern.
    Speed,
    /// The beam's diameter at the aperture, millimetres.
    Beam,
    /// How fast the beam widens, milliradians (full angle).
    Divergence,
    /// 1 when the diodes are only on or off (TTL), 0 when they dim (analog).
    Ttl,
    /// The most of its power it may emit, 0 to 1.
    Cap,
    /// 1 to mirror the output across or up, or to swap the two axes: how the
    /// box hangs and how its DAC is wired.
    FlipX,
    FlipY,
    Swap,
    /// How much of the DAC's range the full scan angle takes, 0 to 1.
    SizeX,
    SizeY,
    /// Where the scan's middle sits in the DAC's range, −1 to 1.
    OffsetX,
    OffsetY,
    /// How many points later the colour is sent than the place it belongs
    /// to: the mirrors lag the diodes.
    ColourDelay,
    /// The drive level at which each diode starts to give light, 0 to 1.
    FloorRed,
    FloorGreen,
    FloorBlue,
}

impl Key {
    /// Whether the setting is the box's own: how its picture is turned,
    /// sized and placed for the way it hangs and is wired. The rest say what
    /// kind of projector it is, and a venue's projectors share those unless
    /// one holds its own.
    #[must_use]
    pub fn is_own(self) -> bool {
        matches!(
            self,
            Key::FlipX
                | Key::FlipY
                | Key::Swap
                | Key::SizeX
                | Key::SizeY
                | Key::OffsetX
                | Key::OffsetY
        )
    }

    pub const ALL: [Key; 23] = [
        Key::PowerRed,
        Key::PowerGreen,
        Key::PowerBlue,
        Key::WavelengthRed,
        Key::WavelengthGreen,
        Key::WavelengthBlue,
        Key::Angle,
        Key::Speed,
        Key::Beam,
        Key::Divergence,
        Key::Ttl,
        Key::Cap,
        Key::FlipX,
        Key::FlipY,
        Key::Swap,
        Key::SizeX,
        Key::SizeY,
        Key::OffsetX,
        Key::OffsetY,
        Key::ColourDelay,
        Key::FloorRed,
        Key::FloorGreen,
        Key::FloorBlue,
    ];

    /// The node parameter it is stored under.
    #[must_use]
    pub fn param(self) -> &'static str {
        match self {
            Key::PowerRed => "laser_power_red",
            Key::PowerGreen => "laser_power_green",
            Key::PowerBlue => "laser_power_blue",
            Key::WavelengthRed => "laser_wavelength_red",
            Key::WavelengthGreen => "laser_wavelength_green",
            Key::WavelengthBlue => "laser_wavelength_blue",
            Key::Angle => "laser_angle",
            Key::Speed => "laser_speed",
            Key::Beam => "laser_beam",
            Key::Divergence => "laser_divergence",
            Key::Ttl => "laser_ttl",
            Key::Cap => "laser_cap",
            Key::FlipX => "laser_flip_x",
            Key::FlipY => "laser_flip_y",
            Key::Swap => "laser_swap",
            Key::SizeX => "laser_size_x",
            Key::SizeY => "laser_size_y",
            Key::OffsetX => "laser_offset_x",
            Key::OffsetY => "laser_offset_y",
            Key::ColourDelay => "laser_colour_delay",
            Key::FloorRed => "laser_floor_red",
            Key::FloorGreen => "laser_floor_green",
            Key::FloorBlue => "laser_floor_blue",
        }
    }

    /// The setting stored under `param`.
    #[must_use]
    pub fn of(param: &str) -> Option<Key> {
        Key::ALL.into_iter().find(|key| key.param() == param)
    }

    /// The least and most it may be.
    #[must_use]
    pub fn range(self) -> [f64; 2] {
        match self {
            Key::PowerRed | Key::PowerGreen | Key::PowerBlue => [0., 100.],
            Key::WavelengthRed | Key::WavelengthGreen | Key::WavelengthBlue => [380., 780.],
            Key::Angle => [1., 90.],
            Key::Speed => [1., 100.],
            Key::Beam => [0.5, 50.],
            Key::Divergence => [0.1, 20.],
            Key::Ttl | Key::FlipX | Key::FlipY | Key::Swap => [0., 1.],
            Key::Cap | Key::SizeX | Key::SizeY => [0., 1.],
            Key::OffsetX | Key::OffsetY => [-1., 1.],
            Key::ColourDelay => [0., 32.],
            Key::FloorRed | Key::FloorGreen | Key::FloorBlue => [0., 0.9],
        }
    }

    /// Whether it is a switch, stored as 0 or 1.
    #[must_use]
    pub fn is_switch(self) -> bool {
        matches!(self, Key::Ttl | Key::FlipX | Key::FlipY | Key::Swap)
    }
}

/// A projector's settings, one value per [`Key`].
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings([f64; Key::ALL.len()]);

/// A size of projector to start from: typical RGB diode powers, scanner and
/// beam for its class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Watts1,
    Watts3,
    Watts5,
    Watts10,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::Watts1,
        Preset::Watts3,
        Preset::Watts5,
        Preset::Watts10,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Preset::Watts1 => "1 W RGB",
            Preset::Watts3 => "3 W RGB",
            Preset::Watts5 => "5 W RGB",
            Preset::Watts10 => "10 W RGB",
        }
    }

    /// Red, green and blue watts, scanner kpps, beam millimetres and
    /// divergence milliradians.
    fn values(self) -> ([f64; 3], f64, f64, f64) {
        match self {
            Preset::Watts1 => ([0.25, 0.3, 0.45], 20., 3., 1.5),
            Preset::Watts3 => ([0.75, 1.0, 1.25], 30., 4., 1.2),
            Preset::Watts5 => ([1.2, 1.6, 2.2], 30., 5., 1.0),
            Preset::Watts10 => ([2.5, 3.0, 4.5], 40., 6., 1.0),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings::preset(Preset::Watts3)
    }
}

impl Settings {
    /// `preset`'s light, scanner and beam, untrimmed and uncapped.
    #[must_use]
    pub fn preset(preset: Preset) -> Self {
        let mut settings = Settings([0.; Key::ALL.len()]);
        let (power, speed, beam, divergence) = preset.values();
        for (key, value) in [
            (Key::PowerRed, power[0]),
            (Key::PowerGreen, power[1]),
            (Key::PowerBlue, power[2]),
            (Key::WavelengthRed, 638.),
            (Key::WavelengthGreen, 520.),
            (Key::WavelengthBlue, 450.),
            (Key::Angle, 60.),
            (Key::Speed, speed),
            (Key::Beam, beam),
            (Key::Divergence, divergence),
            (Key::Cap, 1.),
            (Key::SizeX, 1.),
            (Key::SizeY, 1.),
        ] {
            settings.set(key, value);
        }
        settings
    }

    /// The settings a node's parameters hold, the rest as the default's.
    #[must_use]
    pub fn read(params: &Params) -> Self {
        Settings::default().over(params, |_| true)
    }

    /// What a venue's projectors share: the kind of projector they are, as
    /// its root node holds it.
    #[must_use]
    pub fn shared(root: &Params) -> Self {
        Settings::default().over(root, |key| !key.is_own())
    }

    /// These settings with a projector's own over them.
    #[must_use]
    pub fn with(self, projector: &Params) -> Self {
        self.over(projector, |_| true)
    }

    /// Whether a projector holds a kind of its own, and so does not follow
    /// the venue's.
    #[must_use]
    pub fn has_kind(projector: &Params) -> bool {
        Key::ALL
            .into_iter()
            .any(|key| !key.is_own() && projector.lookup(key.param()).is_some())
    }

    fn over(mut self, params: &Params, take: impl Fn(Key) -> bool) -> Self {
        for key in Key::ALL.into_iter().filter(|key| take(*key)) {
            if let Some(value) = params.lookup(key.param()) {
                self.set(key, value);
            }
        }
        self
    }

    /// What a preset sets: a projector's light, scanner and beam.
    pub const SHAPE: [Key; 6] = [
        Key::PowerRed,
        Key::PowerGreen,
        Key::PowerBlue,
        Key::Speed,
        Key::Beam,
        Key::Divergence,
    ];

    /// The preset whose light, scanner and beam these are, if any.
    #[must_use]
    pub fn matches(&self) -> Option<Preset> {
        Preset::ALL.into_iter().find(|preset| {
            let preset = Settings::preset(*preset);
            Self::SHAPE
                .iter()
                .all(|key| preset.get(*key) == self.get(*key))
        })
    }

    #[must_use]
    pub fn get(&self, key: Key) -> f64 {
        self.0[key as usize]
    }

    /// Set `key`, kept inside its range; a switch is 0 or 1.
    pub fn set(&mut self, key: Key, value: f64) {
        let [lo, hi] = key.range();
        let value = if value.is_finite() { value } else { lo };
        self.0[key as usize] = if key.is_switch() {
            f64::from(value >= 0.5)
        } else {
            value.clamp(lo, hi)
        };
    }

    #[must_use]
    pub fn is_on(&self, key: Key) -> bool {
        self.get(key) >= 0.5
    }

    /// Red, green and blue watts.
    #[must_use]
    pub fn power(&self) -> [f64; 3] {
        [Key::PowerRed, Key::PowerGreen, Key::PowerBlue].map(|key| self.get(key))
    }

    fn wavelength(&self) -> [f64; 3] {
        [
            Key::WavelengthRed,
            Key::WavelengthGreen,
            Key::WavelengthBlue,
        ]
        .map(|key| self.get(key))
    }

    /// Points a second.
    #[must_use]
    pub fn rate(&self) -> u32 {
        (self.get(Key::Speed) * 1000.).round() as u32
    }

    /// What each diode gives at full drive as an eye sees it: XYZ scaled so
    /// Y is lumens, red, green and blue in columns.
    fn primaries(&self) -> DMat3 {
        let (power, wavelength) = (self.power(), self.wavelength());
        DMat3::from_cols_array_2d(&std::array::from_fn(|diode| {
            observer(wavelength[diode]).map(|c| c * power[diode] * PEAK_LUMENS_PER_WATT)
        }))
    }

    /// What each diode gives at full drive, in linear sRGB scaled so its
    /// luminance is lumens: red, green and blue in turn. A laser line lies
    /// outside sRGB, so a component can be below zero; the sum over lit
    /// diodes is what a screen clips.
    #[must_use]
    pub fn light(&self) -> [[f64; 3]; 3] {
        let srgb = DMat3::from_cols_array_2d(&XYZ_TO_SRGB).transpose();
        (srgb * self.primaries()).to_cols_array_2d()
    }

    /// The lumens of its brightest white.
    #[must_use]
    pub fn lumens(&self) -> f64 {
        let white = self.drive([1.; 3]);
        (self.primaries() * DVec3::from(white)).y
    }

    /// How hard to drive each diode, 0 to 1, for a clip's colour (linear
    /// sRGB, 0 to 1) to be that colour in the room: white is the brightest
    /// white its diodes mix, every other colour in proportion, all under the
    /// power cap. A TTL projector's diodes are on or off.
    #[must_use]
    pub fn drive(&self, colour: [f64; 3]) -> [f64; 3] {
        let cap = self.get(Key::Cap);
        let drive = match self.mix() {
            Some(mix) => (mix * DVec3::from(colour)).to_array(),
            // A projector missing a diode cannot mix: each one it has
            // follows its own channel.
            None => {
                let power = self.power();
                std::array::from_fn(|c| if power[c] > 0. { colour[c] } else { 0. })
            }
        };
        drive.map(|level| {
            let level = level.clamp(0., 1.);
            if self.is_on(Key::Ttl) {
                f64::from(level >= 0.5) * cap
            } else {
                level * cap
            }
        })
    }

    /// Linear sRGB to drive levels, scaled so white's hardest-driven diode
    /// is at 1. `None` when a diode is missing.
    fn mix(&self) -> Option<DMat3> {
        let primaries = self.primaries();
        if primaries.determinant().abs() < 1e-9 {
            return None;
        }
        let mix = primaries.inverse() * DMat3::from_cols_array_2d(&SRGB_TO_XYZ).transpose();
        let white = mix * DVec3::ONE;
        (white.max_element() > 0.).then(|| mix * (1. / white.max_element()))
    }

    /// Half the scan angle, degrees: one galvo unit.
    #[must_use]
    pub fn reach(&self) -> f64 {
        self.get(Key::Angle) / 2.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_with_no_settings_is_the_default_and_set_keys_override_it() {
        assert_eq!(Settings::read(&Params::default()), Settings::default());
        let mut params = Params::default();
        params.set(Key::Angle.param(), 40.);
        params.set(Key::PowerGreen.param(), 1e9);
        let settings = Settings::read(&params);
        assert_eq!(settings.get(Key::Angle), 40.);
        assert_eq!(settings.get(Key::PowerGreen), Key::PowerGreen.range()[1]);
        assert_eq!(settings.matches(), None);
        assert_eq!(Settings::default().matches(), Some(Preset::Watts3));
    }

    #[test]
    fn white_is_white_and_as_bright_as_the_diodes_allow() {
        let settings = Settings::default();
        let drive = settings.drive([1.; 3]);
        assert!((drive.iter().copied().fold(0., f64::max) - 1.).abs() < 1e-9);
        // The mix of the three diodes at those levels is neutral in sRGB.
        let light = settings.light();
        let mixed: [f64; 3] =
            std::array::from_fn(|c| (0..3).map(|diode| light[diode][c] * drive[diode]).sum());
        let mean = mixed.iter().sum::<f64>() / 3.;
        assert!(
            mixed.iter().all(|c| (c / mean - 1.).abs() < 0.02),
            "{mixed:?}"
        );
        // Half the colour is half the light, and every sRGB colour is in reach.
        let half = settings.drive([0.5; 3]);
        assert!(drive
            .iter()
            .zip(half)
            .all(|(a, b)| (a / 2. - b).abs() < 1e-9));
        for colour in [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.], [1., 1., 0.]] {
            assert!(settings
                .drive(colour)
                .iter()
                .all(|d| (0. ..=1.).contains(d)));
        }
    }

    #[test]
    fn a_watt_of_green_is_brighter_than_a_watt_of_red_or_blue() {
        let mut settings = Settings::default();
        for key in [Key::PowerRed, Key::PowerGreen, Key::PowerBlue] {
            settings.set(key, 1.);
        }
        let luminance = |diode: [f64; 3]| 0.2126 * diode[0] + 0.7152 * diode[1] + 0.0722 * diode[2];
        let [red, green, blue] = settings.light().map(luminance);
        assert!(green > 3. * red && red > 3. * blue, "{red} {green} {blue}");
        // A 3 W show laser's white is a few hundred lumens.
        let lumens = Settings::default().lumens();
        assert!((200. ..800.).contains(&lumens), "{lumens}");
    }

    #[test]
    fn the_cap_and_ttl_limit_what_is_driven() {
        let mut settings = Settings::default();
        settings.set(Key::Cap, 0.25);
        assert!(settings.drive([1.; 3]).iter().all(|d| *d <= 0.25 + 1e-9));
        settings.set(Key::Cap, 1.);
        settings.set(Key::Ttl, 1.);
        let drive = settings.drive([0.9, 0.1, 0.0]);
        assert!(drive.iter().all(|d| *d == 0. || *d == 1.), "{drive:?}");
    }

    #[test]
    fn a_projector_missing_a_diode_still_lights_the_ones_it_has() {
        let mut settings = Settings::default();
        settings.set(Key::PowerRed, 0.);
        settings.set(Key::PowerBlue, 0.);
        assert_eq!(settings.drive([0.3, 0.6, 0.9]), [0., 0.6, 0.]);
    }
}
