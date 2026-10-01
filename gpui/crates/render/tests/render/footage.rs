//! The footage look (`footage.rs`) and the strobe on the frame clock
//! (`strobe.rs`), drawn.
//!
//! ```sh
//! cargo test -p luma-render --test render footage:: -- --test-threads=1
//! ```
//!
//! The golden strobe over a dark, hazy stage: the beam is what the flash
//! lights.

use luma_render::scene_desc::{Catalogue, Footage, Look, Scene};
use luma_render::{build_frame_at, footage, Frame, Renderer};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 96;

fn catalogue() -> Catalogue {
    Catalogue::load(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens/scenes.json"),
    )
    .unwrap()
}

/// The strobe-duty golden, 10 Hz flashes, with `footage`.
fn strobe_scene(catalogue: &Catalogue, footage: Footage) -> Scene {
    let mut scene = catalogue
        .scenes
        .iter()
        .find(|scene| scene.id == "strobe-duty")
        .expect("the strobe golden")
        .clone();
    scene.render.look = Look {
        footage,
        ..Look::NEUTRAL
    };
    scene
}

/// The frame that ends at clock `end`, `interval` after the one before it.
fn frame(catalogue: &Catalogue, scene: &Scene, end: f64, interval: f64) -> Frame {
    let mut library = crate::common::library();
    build_frame_at(
        scene,
        &catalogue.definitions,
        &|id, head| scene.primitive(id, head),
        footage::moment(end, interval, 0.0),
        &mut library,
    )
    .unwrap()
}

fn quiet_camera() -> Footage {
    Footage {
        enabled: true,
        noise: 0.0,
        handheld: 0.0,
        bass: 0.0,
    }
}

#[test]
fn a_frame_shows_the_share_of_its_interval_the_gate_is_on() {
    let catalogue = catalogue();
    let scene = strobe_scene(&catalogue, Footage::OFF);
    let mut steady = scene.clone();
    for state in steady.state.values_mut() {
        state.strobe = 0.0;
    }
    let mut renderer = Renderer::new().unwrap();
    let mut mean = |scene: &Scene, end: f64, interval: f64| {
        let frame = frame(&catalogue, scene, end, interval);
        crate::common::mean_rgb(&renderer.render(&frame, WIDTH, HEIGHT, 16).unwrap())
    };
    let full = mean(&steady, 0.14, footage::FRAME_S);
    // At 10 Hz the gate is on over [0.1, 0.15]. Frames of 30, 45, 60 and
    // 144 per second inside it all show the steady light's brightness...
    for interval in [1.0 / 30.0, 1.0 / 45.0, 1.0 / 60.0, 1.0 / 144.0] {
        let lit = mean(&scene, 0.1 + interval + 0.005, interval);
        assert!(
            (lit - full).abs() < 0.01 * full,
            "{interval}: {lit} vs steady {full}"
        );
    }
    // ...a frame inside the off half shows none of it...
    let dark = mean(&scene, 0.19, footage::FRAME_S);
    assert!(
        full > dark + 1.0,
        "the flash lights the stage: {full} vs {dark}"
    );
    // ...and a frame across the edge at 0.15 shows part of it.
    let part = mean(&scene, 0.15 + footage::FRAME_S / 2.0, footage::FRAME_S);
    assert!(
        dark + 0.5 < part && part < full - 0.5,
        "half on: {dark} < {part} < {full}"
    );
}

#[test]
fn sensor_noise_repeats_per_frame_and_grows_with_the_gain() {
    let catalogue = catalogue();
    let mut renderer = Renderer::new().unwrap();
    // The dark stage between two flashes: what shows is the read noise.
    let mut picture = |noise: f32, ev: f32, end: f64| {
        let mut scene = strobe_scene(
            &catalogue,
            Footage {
                noise,
                ..quiet_camera()
            },
        );
        scene.render.look.exposure.ev = ev;
        let frame = frame(&catalogue, &scene, end, footage::FRAME_S);
        renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap()
    };
    let grain = |a: &[u8], b: &[u8]| {
        a.iter()
            .zip(b)
            .map(|(a, b)| f64::from(a.abs_diff(*b)))
            .sum::<f64>()
            / a.len() as f64
    };
    let clean = picture(0.0, 0.0, 0.05);
    let noisy = picture(1.0, 0.0, 0.05);
    assert_eq!(noisy, picture(1.0, 0.0, 0.05), "a frame's grain is its own");
    assert!(grain(&clean, &noisy) > 0.0, "the sensor adds grain");
    assert_ne!(
        noisy,
        picture(1.0, 0.0, 0.06),
        "the next frame's grain differs"
    );
    let gained = grain(&picture(0.0, 2.0, 0.05), &picture(1.0, 2.0, 0.05));
    assert!(
        gained > grain(&clean, &noisy),
        "two stops of gain show more grain"
    );
}
