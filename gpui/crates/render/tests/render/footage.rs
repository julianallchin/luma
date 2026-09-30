//! The footage look's shutter (`footage.rs`, `strobe.rs`): a frame exposed
//! over several whole moments.
//!
//! ```sh
//! cargo test -p luma-render --test render footage:: -- --test-threads=1
//! ```
//!
//! The golden strobe over a dark, hazy stage: the beam is what the flash
//! lights.

use luma_render::frame::Moment;
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

/// The frame that ends at clock `end`, exposed over `subframes` moments.
fn exposure(catalogue: &Catalogue, scene: &Scene, end: f64, subframes: u32) -> Frame {
    let mut library = crate::common::library();
    let moments = footage::moments(
        &scene.render.look.footage,
        end,
        footage::FRAME_S,
        subframes,
        |_| 0.0,
    );
    Frame::exposure(
        moments
            .into_iter()
            .map(|moment: Moment| {
                build_frame_at(
                    scene,
                    &catalogue.definitions,
                    &|id, head| scene.primitive(id, head),
                    moment,
                    &mut library,
                )
                .unwrap()
            })
            .collect(),
    )
}

fn global_shutter() -> Footage {
    Footage {
        enabled: true,
        readout_ms: 0.0,
        noise: 0.0,
        handheld: 0.0,
        bass: 0.0,
        ..Footage::OFF
    }
}

#[test]
fn a_flash_is_as_bright_at_two_subframes_as_at_sixteen() {
    let catalogue = catalogue();
    let scene = strobe_scene(&catalogue, global_shutter());
    let mut renderer = Renderer::new().unwrap();
    // A 180 degree shutter at 60 fps is open for the last 8.3 ms before the
    // frame's end. Ending at 0.103 s it catches 3 ms of the flash at 0.1 s;
    // ending at 0.05 s it catches none.
    let mut mean = |end: f64, subframes: u32| {
        let frame = exposure(&catalogue, &scene, end, subframes);
        assert_eq!(frame.moments().count(), subframes as usize);
        // The same haze passes in all, shared between the moments.
        crate::common::mean_rgb(&renderer.render(&frame, WIDTH, HEIGHT, 16).unwrap())
    };
    let dark = mean(0.05, 2);
    let two = mean(0.103, 2);
    let sixteen = mean(0.103, 16);
    assert!(
        two > dark + 1.0,
        "the flash lights the stage: {two} vs {dark}"
    );
    assert!(
        (two - sixteen).abs() < 0.02 * (two - dark),
        "2 subframes {two}, 16 subframes {sixteen}, dark {dark}"
    );
}

#[test]
fn a_shutter_of_equal_moments_is_that_moment() {
    let catalogue = catalogue();
    let scene = strobe_scene(&catalogue, global_shutter());
    let mut library = crate::common::library();
    let moment = footage::moments(
        &scene.render.look.footage,
        0.103,
        footage::FRAME_S,
        1,
        |_| 0.0,
    )[0];
    let mut build = || {
        build_frame_at(
            &scene,
            &catalogue.definitions,
            &|id, head| scene.primitive(id, head),
            moment,
            &mut library,
        )
        .unwrap()
    };
    let alone = build();
    let twice = Frame::exposure(vec![build(), build()]);
    let mut renderer = Renderer::new().unwrap();
    // Sixteen haze passes for the lone moment, eight for each of the pair:
    // only the jitter differs, and it averages out.
    let alone = crate::common::mean_rgb(&renderer.render(&alone, WIDTH, HEIGHT, 16).unwrap());
    let twice = crate::common::mean_rgb(&renderer.render(&twice, WIDTH, HEIGHT, 16).unwrap());
    assert!(alone > 1.0, "the flash lights the haze: {alone}");
    assert!((alone - twice).abs() < 0.02 * alone, "{alone} vs {twice}");
}
