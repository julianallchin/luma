//! The rows of the view-settings popover's Camera and View pages and of the
//! venue's haze, and the toggle, scrub and choice rows they are built from.
use super::*;
use luma_lib::settings::RENDER_SCALE_RANGE;

/// The room's haze: venue truth, set on the venue page.
pub(super) fn haze_rows(state: &Visualizer, app: &Entity<Luma>) -> Div {
    let controls = &state.render_controls;
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(view_toggle(
            state,
            app,
            "Haze",
            controls.haze.enabled,
            ViewToggle::Haze,
        ))
        .child(view_value(
            app,
            "Haze density",
            controls.haze.density,
            0.,
            luma_scene::environment::VenueHaze::MAX_DENSITY,
            ViewValue::HazeDensity,
        ))
        .child(view_value(
            app,
            "Cloudiness",
            controls.haze.appearance.cloudiness,
            0.,
            1.,
            ViewValue::Cloudiness,
        ))
        .child(view_value(
            app,
            "Cloud size (m)",
            controls.haze.appearance.cloud_size,
            0.5,
            20.,
            ViewValue::CloudSize,
        ))
        .child(view_value(
            app,
            "Turbulence",
            controls.haze.appearance.turbulence,
            0.,
            1.,
            ViewValue::Turbulence,
        ))
        .child(view_value(
            app,
            "Wind speed (m/s)",
            controls.haze.appearance.wind_speed,
            0.,
            10.,
            ViewValue::WindSpeed,
        ))
        .child(view_value(
            app,
            "Wind direction (°)",
            controls.haze.appearance.wind_direction,
            0.,
            360.,
            ViewValue::WindDirection,
        ))
}

/// The View page: what the stage costs to draw, and the chrome drawn over it.
pub(super) fn view_page(state: &Visualizer, app: &Entity<Luma>) -> impl IntoElement {
    let controls = &state.render_controls;
    // The cost knobs sit together; the percent last because its caption
    // belongs directly under it.
    let cost = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(quality_row(controls.quality, app))
        .child(view_value(
            app,
            "Render scale (%)",
            f32::from(controls.render_scale_percent),
            f32::from(*RENDER_SCALE_RANGE.start()),
            f32::from(*RENDER_SCALE_RANGE.end()),
            ViewValue::RenderScale,
        ))
        .children(render_size_caption(state));
    let chrome = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(view_toggle(
            state,
            app,
            "Grid",
            controls.grid_enabled,
            ViewToggle::Grid,
        ))
        .child(view_toggle(
            state,
            app,
            "Gizmos",
            controls.gizmos_enabled,
            ViewToggle::Gizmos,
        ));
    page([cost, chrome])
}

/// The Camera page: how scene light becomes the picture — exposure and tone,
/// the glare around hot sources, and the footage look
/// ([`scene_desc::Look`]).
pub(super) fn camera_page(state: &Visualizer, app: &Entity<Luma>) -> impl IntoElement {
    page([
        exposure_controls(state, app),
        glare_controls(state, app),
        footage_controls(state, app),
    ])
}

/// A page of the view settings card: its groups of rows, a rule between them.
fn page<const N: usize>(groups: [Div; N]) -> Div {
    let mut page = div().flex().flex_col().gap(px(12.));
    for (i, group) in groups.into_iter().enumerate() {
        if i > 0 {
            page = page.child(luma_ui::float::divider());
        }
        page = page.child(group);
    }
    page
}

/// Tone curve and exposure.
fn exposure_controls(state: &Visualizer, app: &Entity<Luma>) -> Div {
    let look = state.render_controls.look;
    let mut tone = luma_ui::float::segmented();
    for (name, curve) in [
        ("AgX", scene_desc::ToneCurve::Agx),
        ("Punchy", scene_desc::ToneCurve::AgxPunchy),
        ("ACES", scene_desc::ToneCurve::Aces),
    ] {
        let app = app.clone();
        tone = tone.child(
            luma_ui::float::segment(name, look.tone == curve, name)
                .id(gpui::ElementId::Name(format!("tone-{name}").into()))
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| this.set_view_tone(curve, cx));
                })
                .agent_node(Role::Toggle, name),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(choice_row(
            "Tone",
            tone.agent_node(Role::Card, "Tone curve"),
        ))
        .child(view_toggle(
            state,
            app,
            "Auto exposure",
            look.exposure.auto,
            ViewToggle::AutoExposure,
        ))
        // Stops on what the meter reads, or on where it was switched off.
        .child(view_value(
            app,
            "Exposure (EV)",
            look.exposure.ev,
            *scene_desc::Exposure::RANGE.start(),
            *scene_desc::Exposure::RANGE.end(),
            ViewValue::Exposure,
        ))
}

/// The glare around hot sources. Its shape shows only while there is any.
fn glare_controls(state: &Visualizer, app: &Entity<Luma>) -> Div {
    let glare = state.render_controls.look.glare;
    let shape = glare.on().then(|| {
        let mut style = luma_ui::float::segmented().w_full();
        for (name, choice) in [
            ("Bloom", scene_desc::GlareStyle::Bloom),
            ("Iris", scene_desc::GlareStyle::Aperture),
            ("Eye", scene_desc::GlareStyle::Eye),
            ("Star", scene_desc::GlareStyle::Star),
        ] {
            let app = app.clone();
            style = style.child(
                luma_ui::float::segment(name, glare.style == choice, name)
                    .id(gpui::ElementId::Name(format!("glare-{name}").into()))
                    .on_click(move |_, _, cx| {
                        app.update(cx, |this, cx| this.set_view_glare_style(choice, cx));
                    })
                    .agent_node(Role::Toggle, name),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(style.agent_node(Role::Card, "Glare style"))
            .child(view_value(
                app,
                "Threshold",
                glare.threshold,
                *GLARE_THRESHOLD.start(),
                *GLARE_THRESHOLD.end(),
                ViewValue::GlareThreshold,
            ))
            .child(view_value(
                app,
                "Diffraction",
                glare.star,
                0.,
                1.,
                ViewValue::Star,
            ))
    });
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(view_value(
            app,
            "Glare",
            glare.strength,
            0.,
            GLARE_MAX,
            ViewValue::Glare,
        ))
        .children(shape)
}

/// The footage look: what a video camera in the room would record —
/// [`scene_desc::Footage`]. Its dials show only while it is on.
fn footage_controls(state: &Visualizer, app: &Entity<Luma>) -> Div {
    let footage = state.render_controls.look.footage;
    let dials = footage.enabled.then(|| {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(view_value(
                app,
                "Sensor noise",
                footage.noise,
                0.,
                1.,
                ViewValue::Noise,
            ))
            .child(view_value(
                app,
                "Handheld shake",
                footage.handheld,
                0.,
                1.,
                ViewValue::Handheld,
            ))
            .child(view_value(
                app,
                "Bass shake",
                footage.bass,
                0.,
                1.,
                ViewValue::BassShake,
            ))
    });
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(view_toggle(
            state,
            app,
            "Footage look",
            footage.enabled,
            ViewToggle::Footage,
        ))
        .children(dials)
}

/// The pixels the stage is actually rendering, under the percent that asks for
/// them.
///
/// The percent alone can mislead: [`RENDER_BUDGET_PIXELS`] clamps a large window
/// below it, so 100% on a fullscreen retina display is not native and a control
/// that only said "100" would be claiming otherwise. Quiet — [`float::label`],
/// the same 11px the card's own legends use — because it is a readout, not a
/// control, and absent until a frame has been laid out, because until then there
/// is no honest number to print.
fn render_size_caption(state: &Visualizer) -> Option<impl IntoElement> {
    let (width, height) = state.stage.borrow().render_size?;
    Some(
        luma_ui::float::label(format!("Rendering {width} × {height}"))
            .agent_node(Role::Text, format!("Rendering {width} × {height}")),
    )
}

pub(super) fn view_toggle(
    state: &Visualizer,
    app: &Entity<Luma>,
    label: &'static str,
    checked: bool,
    control: ViewToggle,
) -> impl IntoElement {
    use luma_ui::node::AgentNode as _;
    let index = match control {
        ViewToggle::Haze => 0,
        ViewToggle::Grid => 1,
        ViewToggle::Gizmos => 2,
        ViewToggle::AutoExposure => 3,
        ViewToggle::ReflectionProbes => 4,
        ViewToggle::ShowProbes => 5,
        ViewToggle::Footage => 6,
    };
    let t = state.settings_motion.borrow_mut().switches[index].sample(checked);
    let app = app.clone();
    div()
        .id(label)
        .flex()
        .items_center()
        .gap(px(8.))
        .justify_between()
        .h(px(28.))
        .cursor_pointer()
        .child(div().text_size(px(12.)).child(label))
        .child(luma_ui::float::switch(t))
        .on_click(move |_, _, cx| {
            app.update(cx, |this, cx| this.toggle_view_control(control, cx));
        })
        .agent_node(Role::Toggle, label)
        .agent_focused(checked)
}

/// A labelled value scrub on the floating settings surface.
fn view_value(
    app: &Entity<Luma>,
    label: &'static str,
    value: f32,
    min: f32,
    max: f32,
    control: ViewValue,
) -> Div {
    let (step, power) = match control {
        ViewValue::HazeDensity => (0.001, 2.0),
        // A whole percent, linearly: the quantity is already a percentage, so a
        // power curve would only make the number under the hand lie about where
        // the hand is.
        ViewValue::RenderScale => (1.0, 1.0),
        // A tenth of a stop: finer than anyone can see, coarse enough that a
        // drag does not stop on 0.37.
        ViewValue::Exposure => (0.1, 1.0),
        _ => (0.01, 1.0),
    };
    let app = app.clone();
    scrub_row(label, value, min..=max, step, power, move |value, cx| {
        app.update(cx, |this, cx| this.set_view_value(control, value, cx));
    })
}

/// "Quality: High / Medium / Low", the renderer's cost level.
fn quality_row(quality: scene_desc::Quality, app: &Entity<Luma>) -> Div {
    let mut track = luma_ui::float::segmented();
    for (name, level) in [
        ("High", scene_desc::Quality::High),
        ("Medium", scene_desc::Quality::Medium),
        ("Low", scene_desc::Quality::Low),
    ] {
        let app = app.clone();
        track = track.child(
            luma_ui::float::segment(name, quality == level, name)
                .id(gpui::ElementId::Name(format!("quality-{name}").into()))
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| this.set_view_quality(level, cx));
                })
                .agent_node(Role::Toggle, name),
        );
    }
    choice_row("Quality", track.agent_node(Role::Card, "Quality"))
}

/// The scrub row's shape with a segmented choice where the scrub would be.
fn choice_row(label: &'static str, track: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(div().text_size(px(12.)).child(label))
        .child(track)
}

/// The one row shape of the floating settings surface: a label on the left
/// and a value scrub on the right. `on_change` gets the value already clamped
/// to `range` and landed on `step`.
pub(super) fn scrub_row(
    label: &'static str,
    value: f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    power: f64,
    on_change: impl Fn(f32, &mut gpui::App) + 'static,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(div().text_size(px(12.)).child(label))
        .child(
            luma_ui::float::scrub_with_power(
                label,
                value.into(),
                f64::from(*range.start())..=f64::from(*range.end()),
                step,
                76.0,
                power,
                move |value, _, cx| on_change(value as f32, cx),
            )
            .agent_node(Role::Slider, label),
        )
}
