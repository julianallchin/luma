//! The viewport's runtime renderer dials — sun, environment, haze, camera
//! look, chrome — and how they resolve into one frame's render settings.
use super::*;
use luma_lib::settings::{AppSettings, RENDER_SCALE_RANGE};

/// Runtime renderer controls. They belong to the viewport, not persisted venue
/// data, so experimentation cannot silently rewrite a show.
///
/// `PartialEq` because the whole set rides inside [`IdleKey`]: a curated
/// field list there would silently miss every dial added later.
#[derive(Clone, PartialEq)]
pub(super) struct RenderControls {
    /// The room's environment, unchanged by score playback.
    ///
    /// Every field below it is this value's *fill*, resolved once through
    /// [`house::fill`] and then free to be overridden by hand. This one is not
    /// a fill: it is what hangs the house rig and what supplies the sky, both
    /// of which the frame builder resolves for itself against the room's
    /// bounds, so it is carried whole rather than flattened into dials.
    house: VenueEnvironment,
    sun_enabled: bool,
    sun_azimuth_deg: f32,
    sun_elevation_deg: f32,
    sun_intensity: f32,
    sun_color: [f32; 3],
    sun_shadows: bool,
    sun_shadow_softness: f32,
    environment_enabled: bool,
    background_color: [f32; 3],
    ambient_color: [f32; 3],
    ambient_intensity: f32,
    probe_enabled: bool,
    probe_intensity: f32,
    probe_rotation_deg: f32,
    probe_visible: bool,
    fixture_surface_lighting: bool,
    pub(super) fixture_shadows: bool,
    /// The room's atmosphere — **venue truth**, saved on the venue row and
    /// synced with it, unlike every other dial here. Adopted from the rig and
    /// written back by [`Luma::set_visualizer_haze`].
    pub(super) haze: luma_scene::environment::VenueHaze,
    /// The march's cost, not the room's look: local, unsynced, and deliberately
    /// absent from [`luma_scene::environment::VenueHaze`] — see its doc comment.
    haze_steps: u32,
    haze_resolution: f32,
    /// Editor chrome, not the room. Local device settings (`stage_grid` /
    /// `stage_gizmos`), so they follow the operator across venues.
    pub(super) grid_enabled: bool,
    /// The `stage_frame_rate` setting: whether the corner frame rate shows.
    pub(super) frame_rate_shown: bool,
    pub(super) gizmos_enabled: bool,
    /// What fraction of the element's pixels the stage renders, as a whole
    /// percent — the `render_scale` device setting. It rides here rather than in
    /// [`scene_desc::RenderSettings`] because it is how many pixels the picture
    /// is sampled at, not what is in it, and because sitting here puts it
    /// in [`IdleKey`] for free.
    pub(super) render_scale_percent: u8,
    /// The camera: exposure, tone curve and glare. A device setting
    /// (`stage_look`), like the grid: it is how this machine looks at any
    /// venue.
    pub(super) look: scene_desc::Look,
    /// The renderer's cost level — the `stage_low_quality` device setting.
    /// Low lowers the haze resolution and gobo samples here, the pixel budget
    /// in [`RenderScale::size`], and the rest inside the renderer.
    pub(super) quality: scene_desc::Quality,
    /// The local reflection probes and their debug balls: a session view
    /// choice, never saved.
    pub(super) probes: scene_desc::ProbeView,
}

/// [`scene_desc::DirectionalLight::EDITOR`]'s direction as an azimuth and
/// elevation, in degrees. [`RenderControls`] holds a sun in polar form because that
/// is what a pair of sliders can hold; the catalogue authors it as a vector.
/// One of the two has to convert, and it is this one, because the vector is
/// the shared contract.
fn editor_sun_angles() -> (f32, f32) {
    let [x, y, z] = scene_desc::DirectionalLight::EDITOR.direction;
    (y.atan2(x).to_degrees(), z.atan2(x.hypot(y)).to_degrees())
}

impl Visualizer {
    pub(crate) fn render_settings(&self) -> scene_desc::RenderSettings {
        self.render_controls.settings(self.camera.fov_y_deg)
    }
}

impl RenderControls {
    /// One settings snapshot for the main viewport and both picker previews.
    pub(super) fn settings(&self, fov: f32) -> scene_desc::RenderSettings {
        let mut render =
            scene_desc::RenderSettings::room(self.house, self.haze, fov, self.haze_resolution);
        render.fov = fov;
        render.environment = scene_desc::Environment {
            background: if self.environment_enabled {
                self.background_color
            } else {
                scene_desc::Environment::DARK.background
            },
            ambient_color: self.ambient_color,
            ambient_intensity: self.ambient_intensity,
            probe: self.probe_enabled.then(|| scene_desc::EnvironmentProbe {
                asset: "environments/studio.hdr".into(),
                intensity: self.probe_intensity,
                rotation_deg: self.probe_rotation_deg,
                visible: self.probe_visible,
            }),
        };
        render.sun = self.sun_enabled.then_some(scene_desc::DirectionalLight {
            direction: self.sun_direction(),
            color: self.sun_color,
            intensity: self.sun_intensity,
            shadows: self.sun_shadows,
            shadow_softness: self.sun_shadow_softness,
        });
        // The room's own two halves, which are not fill
        // dials and so are not these controls' to override: the
        // house rig the frame builder hangs once it knows
        // the bounds, and the atmosphere an open-air venue
        // takes its background, ambient and sun from.
        render.house = Some(self.house);
        render.sky = house::fill(self.house).sky;
        // `room` already carried the venue's own haze in. These two are the
        // local cost knobs it deliberately does not hold.
        render.haze.steps = self.haze_steps;
        render.haze.resolution = self.haze_resolution;
        if self.quality == scene_desc::Quality::Low {
            render.haze.steps = render.haze.steps.min(self.quality.haze_steps());
            render.haze.resolution = render.haze.resolution.min(self.quality.haze_resolution());
        }
        render.quality = self.quality;
        render.show_grid = self.grid_enabled;
        render.show_gizmos = self.gizmos_enabled;
        render.fixture_surface_lighting = self.fixture_surface_lighting;
        render.fixture_shadows = self.fixture_shadows;
        render.geometry_shadows = true;
        render.look = self.look;
        render.probes = self.probes;
        render
    }

    pub(super) fn new(environment: VenueEnvironment) -> Self {
        let (azimuth_deg, elevation_deg) = editor_sun_angles();
        let mut controls = Self {
            house: environment,
            sun_enabled: true,
            // The editor key light in polar form. Read off
            // `DirectionalLight::EDITOR` rather than typed again, so the
            // goldens and the stage cannot light the room differently.
            sun_azimuth_deg: azimuth_deg,
            sun_elevation_deg: elevation_deg,
            sun_intensity: scene_desc::DirectionalLight::EDITOR.intensity,
            sun_color: scene_desc::DirectionalLight::EDITOR.color,
            sun_shadows: true,
            sun_shadow_softness: 1.0,
            // A pure dev override, on by default: what the background and the
            // ambient term actually *are* is the environment's answer, written
            // by `set_environment` below.
            environment_enabled: true,
            background_color: scene_desc::Environment::EDITOR.background,
            ambient_color: scene_desc::Environment::EDITOR.ambient_color,
            ambient_intensity: scene_desc::Environment::EDITOR.ambient_intensity,
            probe_enabled: false,
            probe_intensity: 0.8,
            probe_rotation_deg: 0.0,
            probe_visible: false,
            fixture_surface_lighting: true,
            fixture_shadows: true,
            // The venue's own haze arrives with its rig; until then the haze
            // this app has always drawn, which is exactly `Default`.
            haze: luma_scene::environment::VenueHaze::default(),
            haze_steps: 8,
            haze_resolution: luma_render::LIVE_HAZE_RESOLUTION,
            // Written by `apply_settings` below, from the settings table's
            // defaults until the stored rows arrive.
            grid_enabled: false,
            frame_rate_shown: true,
            gizmos_enabled: false,
            render_scale_percent: 0,
            look: scene_desc::Look::STAGE,
            quality: scene_desc::Quality::High,
            probes: scene_desc::ProbeView::default(),
        };
        controls.set_environment(environment);
        controls.apply_settings(&AppSettings::default());
        controls
    }

    /// Take this machine's view preferences from the settings table.
    pub(super) fn apply_settings(&mut self, settings: &AppSettings) {
        self.grid_enabled = settings.stage_grid;
        self.frame_rate_shown = settings.stage_frame_rate;
        self.gizmos_enabled = settings.stage_gizmos;
        self.render_scale_percent = settings.render_scale;
        self.look = scene_desc::Look::stored(&settings.stage_look);
        self.quality = if settings.stage_low_quality {
            scene_desc::Quality::Low
        } else {
            scene_desc::Quality::High
        };
    }

    /// Re-derive the lighting from the environment the room is now drawn
    /// under.
    ///
    /// Exactly the fields [`house::fill`] answers, and no others: everything
    /// else here is an authored tweak, and a stage that reset a hand-set
    /// haze density because the track changed would be throwing away work
    /// nobody asked it to.
    ///
    /// The grid in particular is **not** derived from the room. This method
    /// runs on every light-slider sample, every score change and every rig
    /// reload, so a derived grid would undo the operator's choice. It is a
    /// device setting, and the operator's answer is the only answer.
    pub(super) fn set_environment(&mut self, environment: VenueEnvironment) {
        let fill = house::fill(environment);
        self.house = environment;
        self.background_color = fill.environment.background;
        self.ambient_color = fill.environment.ambient_color;
        self.ambient_intensity = fill.environment.ambient_intensity;
        self.sun_enabled = fill.sun.is_some();
        self.sun_intensity = fill
            .sun
            .map_or(scene_desc::DirectionalLight::EDITOR.intensity, |sun| {
                sun.intensity
            });
    }

    fn sun_direction(&self) -> [f32; 3] {
        let azimuth = self.sun_azimuth_deg.to_radians();
        let elevation = self.sun_elevation_deg.to_radians();
        let horizontal = elevation.cos();
        [
            horizontal * azimuth.cos(),
            horizontal * azimuth.sin(),
            elevation.sin(),
        ]
    }
}

#[derive(Clone, Copy)]
pub(super) enum ViewToggle {
    FixtureShadows,
    Haze,
    Grid,
    Gizmos,
    AutoExposure,
    ReflectionProbes,
    ShowProbes,
    /// The footage look, [`scene_desc::Footage`].
    Footage,
}
#[derive(Clone, Copy)]
pub(super) enum ViewValue {
    HazeDensity,
    Cloudiness,
    CloudSize,
    Turbulence,
    WindSpeed,
    WindDirection,
    /// Not a haze dial: the fraction of the element's pixels the stage renders.
    RenderScale,
    /// Camera dials, [`scene_desc::Look`]: exposure (compensation when auto),
    /// glare strength, glare threshold and star amount.
    Exposure,
    Glare,
    GlareThreshold,
    Star,
    /// Footage dials, [`scene_desc::Footage`]: sensor noise, handheld sway
    /// and bass shake.
    Noise,
    Handheld,
    BassShake,
}

impl ViewValue {
    /// Whether this dial is part of the camera look rather than the room.
    pub(super) fn is_look(self) -> bool {
        matches!(
            self,
            Self::Exposure
                | Self::Glare
                | Self::GlareThreshold
                | Self::Star
                | Self::Noise
                | Self::Handheld
                | Self::BassShake
        )
    }
}

impl RenderControls {
    pub(super) fn toggle(&mut self, control: ViewToggle) {
        match control {
            ViewToggle::FixtureShadows => self.fixture_shadows = !self.fixture_shadows,
            ViewToggle::Haze => {
                self.haze.enabled = !self.haze.enabled;
                self.haze = self.haze.sanitized();
            }
            ViewToggle::Grid => self.grid_enabled = !self.grid_enabled,
            ViewToggle::Gizmos => self.gizmos_enabled = !self.gizmos_enabled,
            ViewToggle::AutoExposure => {
                let exposure = &mut self.look.exposure;
                exposure.auto = !exposure.auto;
                // The one number means compensation in one mode and the
                // exposure itself in the other; zero is the tuned exposure in
                // both, so it is the only value that means the same thing.
                exposure.ev = 0.0;
            }
            ViewToggle::ReflectionProbes => self.probes.enabled = !self.probes.enabled,
            ViewToggle::ShowProbes => self.probes.debug = !self.probes.debug,
            ViewToggle::Footage => {
                self.look.footage.enabled = !self.look.footage.enabled;
            }
        }
    }
    pub(super) fn set(&mut self, control: ViewValue, value: f32) {
        if matches!(control, ViewValue::RenderScale) {
            self.render_scale_percent = clamp_render_scale(value.round() as i64);
            return;
        }
        if control.is_look() {
            let look = &mut self.look;
            match control {
                ViewValue::Exposure => {
                    look.exposure.ev = value.clamp(
                        *scene_desc::Exposure::RANGE.start(),
                        *scene_desc::Exposure::RANGE.end(),
                    );
                }
                ViewValue::Glare => look.glare.strength = value.clamp(0.0, GLARE_MAX),
                ViewValue::GlareThreshold => {
                    look.glare.threshold =
                        value.clamp(*GLARE_THRESHOLD.start(), *GLARE_THRESHOLD.end());
                }
                ViewValue::Star => look.glare.star = value.clamp(0.0, 1.0),
                ViewValue::Noise => look.footage.noise = value,
                ViewValue::Handheld => look.footage.handheld = value,
                ViewValue::BassShake => look.footage.bass = value,
                _ => unreachable!(),
            }
            look.footage = look.footage.sanitized();
            return;
        }
        match control {
            ViewValue::HazeDensity => self.haze.density = value,
            ViewValue::Cloudiness => self.haze.appearance.cloudiness = value,
            ViewValue::CloudSize => self.haze.appearance.cloud_size = value,
            ViewValue::Turbulence => self.haze.appearance.turbulence = value,
            ViewValue::WindSpeed => self.haze.appearance.wind_speed = value,
            ViewValue::WindDirection => self.haze.appearance.wind_direction = value,
            // Handled above; they are not part of the room's atmosphere.
            ViewValue::RenderScale
            | ViewValue::Exposure
            | ViewValue::Glare
            | ViewValue::GlareThreshold
            | ViewValue::Star
            | ViewValue::Noise
            | ViewValue::Handheld
            | ViewValue::BassShake => unreachable!(),
        }
        // The whole value, not just the appearance: `sanitized` is what keeps a
        // swept density inside [`luma_scene::environment::VenueHaze::MAX_DENSITY`], which is
        // now the single answer to how dense a room may be.
        self.haze = self.haze.sanitized();
    }
}

/// The glare strength slider's travel. One is the stage default's full strength;
/// past it the halo is doubled.
pub(super) const GLARE_MAX: f32 = 2.0;
/// The glare threshold's travel, in multiples of diffuse white.
pub(super) const GLARE_THRESHOLD: std::ops::RangeInclusive<f32> = 0.5..=8.0;

/// A percent from anywhere — a slider, a stored setting, an older build — held
/// to the one range the renderer has an answer for.
pub(super) fn clamp_render_scale(percent: i64) -> u8 {
    percent.clamp(
        i64::from(*RENDER_SCALE_RANGE.start()),
        i64::from(*RENDER_SCALE_RANGE.end()),
    ) as u8
}

#[cfg(test)]
mod view_tests {
    use super::*;

    #[test]
    fn view_controls_are_independent_and_bounded() {
        let mut controls = RenderControls::new(VenueEnvironment::default());
        let house = controls.house;
        controls.set(ViewValue::HazeDensity, 9.0);
        assert_eq!(
            controls.haze.density,
            luma_scene::environment::VenueHaze::MAX_DENSITY
        );
        controls.toggle(ViewToggle::FixtureShadows);
        assert!(!controls.fixture_shadows);
        assert_eq!(controls.house, house);
    }

    #[test]
    fn a_fresh_lab_draws_the_default_room_atmosphere() {
        let controls = RenderControls::new(VenueEnvironment::default());
        assert_eq!(controls.haze, luma_scene::environment::VenueHaze::default());
        // The venue's haze, not the default, reaches the renderer — and the two
        // cost knobs the venue deliberately does not carry still do.
        let mut controls = controls;
        controls.haze = luma_scene::environment::VenueHaze {
            enabled: true,
            density: 0.4,
            ..Default::default()
        };
        controls.haze_steps = 13;
        let render = controls.settings(50.0);
        assert_eq!(render.haze.density, 0.4);
        assert_eq!(render.haze.steps, 13);
        assert_eq!(render.haze.resolution, controls.haze_resolution);
    }

    /// The grid is not derived from the room, so a grid switched off stays off
    /// through a light sample, score change or rig reload — all three run
    /// `set_environment`.
    #[test]
    fn changing_the_room_leaves_haze_and_chrome_alone() {
        let mut controls = RenderControls::new(VenueEnvironment::default());
        controls.toggle(ViewToggle::Grid);
        controls.toggle(ViewToggle::Gizmos);
        controls.set(ViewValue::HazeDensity, 0.31);
        controls.set(ViewValue::Cloudiness, 0.7);
        controls.set(ViewValue::RenderScale, 40.0);
        let before = controls.clone();

        for environment in [
            VenueEnvironment::outdoor(40.0),
            VenueEnvironment::indoor(0.0),
            VenueEnvironment::default(),
        ] {
            controls.set_environment(environment);
            assert!(
                !controls.grid_enabled,
                "{environment:?} switched the grid back on"
            );
            assert!(
                !controls.gizmos_enabled,
                "{environment:?} switched gizmos back on"
            );
            assert_eq!(
                controls.haze, before.haze,
                "{environment:?} rewrote the haze"
            );
            assert_eq!(controls.render_scale_percent, before.render_scale_percent);
        }
    }

    #[test]
    fn the_render_percent_is_a_whole_percent_in_range() {
        let mut controls = RenderControls::new(VenueEnvironment::default());
        assert_eq!(controls.render_scale_percent, 100);
        controls.set(ViewValue::RenderScale, 62.4);
        assert_eq!(controls.render_scale_percent, 62);
        controls.set(ViewValue::RenderScale, 1.0);
        assert_eq!(controls.render_scale_percent, 25);
        controls.set(ViewValue::RenderScale, 400.0);
        assert_eq!(controls.render_scale_percent, 100);
        // It is not haze, and touching it must not sanitize haze into range or
        // out of it.
        let haze = controls.haze;
        controls.set(ViewValue::RenderScale, 50.0);
        assert_eq!(controls.haze, haze);
    }

    /// The camera dials reach the renderer, stay in range and leave the room
    /// alone; switching the meter resets the one number that changes meaning.
    #[test]
    fn camera_dials_shape_the_look_and_nothing_else() {
        let mut controls = RenderControls::new(VenueEnvironment::default());
        assert_eq!(controls.settings(50.0).look, scene_desc::Look::STAGE);
        let haze = controls.haze;
        controls.set(ViewValue::Exposure, 9.0);
        assert_eq!(
            controls.look.exposure.ev,
            *scene_desc::Exposure::RANGE.end()
        );
        controls.set(ViewValue::Glare, -1.0);
        assert_eq!(controls.look.glare.strength, 0.0);
        controls.set(ViewValue::GlareThreshold, 100.0);
        assert_eq!(controls.look.glare.threshold, *GLARE_THRESHOLD.end());
        controls.set(ViewValue::Star, 0.25);
        assert_eq!(controls.settings(50.0).look.glare.star, 0.25);
        assert_eq!(controls.haze, haze);

        controls.toggle(ViewToggle::AutoExposure);
        assert!(!controls.look.exposure.auto);
        assert_eq!(controls.look.exposure.ev, 0.0);
    }

    /// The footage look is off by default, turns on with its dials, keeps them
    /// in range and is saved with the rest of the look.
    #[test]
    fn footage_dials_shape_the_look() {
        let mut controls = RenderControls::new(VenueEnvironment::default());
        assert!(!controls.look.footage.enabled);
        controls.toggle(ViewToggle::Footage);
        assert!(controls.settings(50.0).look.footage.enabled);
        controls.set(ViewValue::Noise, 0.5);
        controls.set(ViewValue::Handheld, 2.0);
        controls.set(ViewValue::BassShake, 0.25);
        let footage = controls.look.footage;
        assert_eq!(
            (footage.noise, footage.handheld, footage.bass),
            (0.5, 1.0, 0.25)
        );
        let json = serde_json::to_string(&controls.look).unwrap();
        assert_eq!(scene_desc::Look::stored(&json).footage, footage);
    }
}
