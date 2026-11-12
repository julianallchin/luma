//! Bounced light (`bounce.rs`): what the lit stage sends on to the rest of
//! it, relit every frame.
//!
//! `LUMA_RENDER_CAPTURE_DIR` saves every frame these tests look at.
use std::collections::BTreeMap;

use glam::{Mat4, Vec3};
use luma_render::{
    assets::Library,
    build_frame_with,
    frame::{Draw, FixtureCone, MaterialTextures},
    materials,
    scene_desc::{CameraPose, Geometry, Piece, Quality, RenderSettings, Scene},
    Frame, Renderer,
};
use luma_scene::mesh::{Material, MeshData, Vertex};
use luma_scene::{
    environment::{Floor, VenueEnvironment, VenueHaze},
    luminaire::Lens,
    truss::Procedural,
};

use crate::common::capture;

const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;

fn library() -> Library {
    Library::new(luma_scene::meshes_root())
}

/// A frame of `pieces` in `environment`, from `eye` toward `target` (world
/// metres, Z up), with no haze and no editor overlays.
fn frame_with(
    environment: VenueEnvironment,
    room: Option<[f32; 3]>,
    eye: Vec3,
    target: Vec3,
    quality: Quality,
    pieces: Vec<Piece>,
) -> Frame {
    let mut render = RenderSettings::room(environment, VenueHaze::default(), 50.0, 1.0);
    render.room = room;
    render.haze.enabled = false;
    render.show_grid = false;
    render.show_gizmos = false;
    render.quality = quality;
    // Camera poses are three-space: (x, up, -y).
    let three = |p: Vec3| [p.x, p.z, -p.y];
    let scene = Scene {
        id: "bounce".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: three(eye),
            target: three(target),
        },
        editing: false,
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces,
        state: BTreeMap::new(),
    };
    build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library()).unwrap()
}

/// A light at `from` aimed at `to`: a wash, 15 degrees to the half-peak edge.
fn wash(from: Vec3, to: Vec3, color: Vec3) -> FixtureCone {
    FixtureCone {
        position: from,
        range: 40.0,
        direction: (to - from).normalize(),
        cos_beam: 15.0_f32.to_radians().cos(),
        color,
        intensity: 6.0,
        cos_field: 22.0_f32.to_radians().cos(),
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
        lens: Lens::POINT,
    }
}

/// A unit cube, one flat-shaded quad per face.
fn cube() -> MeshData {
    let faces: [(Vec3, Vec3, Vec3); 6] = [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::NEG_X, Vec3::NEG_Y, Vec3::Z),
        (Vec3::Y, Vec3::NEG_X, Vec3::Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::X, Vec3::NEG_Y),
    ];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (n, u, w) in faces {
        let base = vertices.len() as u32;
        for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = n * 0.5 + u * 0.5 * a + w * 0.5 * b;
            vertices.push(Vertex {
                position: p.to_array(),
                normal: n.to_array(),
                uv: [0.0; 2],
                tangent: [u.x, u.y, u.z, 1.0],
            });
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    MeshData {
        key: "::bounce-test-cube".into(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
}

/// Add boxes to `frame`: each a centre, a size and a material.
fn add_boxes(frame: &mut Frame, boxes: &[(Vec3, Vec3, Material)]) {
    let mesh = frame.meshes.len();
    frame.meshes.push(cube());
    let at = frame.draws.len() - frame.transparent.len();
    for (i, (centre, size, material)) in boxes.iter().enumerate() {
        frame.draws.insert(
            at + i,
            Draw {
                mesh,
                model: Mat4::from_translation(*centre) * Mat4::from_scale(*size),
                material: *material,
                textures: MaterialTextures::default(),
                editor_object: None,
            },
        );
    }
}

/// Where `world` lands in `frame`'s picture, pixels.
fn project(frame: &Frame, world: Vec3) -> (u32, u32) {
    let view_proj = Mat4::perspective_rh(
        frame.camera.fov_y_deg.to_radians(),
        WIDTH as f32 / HEIGHT as f32,
        0.1,
        1000.0,
    ) * Mat4::look_at_rh(frame.camera.eye, frame.camera.target, Vec3::Z);
    let clip = view_proj * world.extend(1.0);
    let x = ((clip.x / clip.w * 0.5 + 0.5) * WIDTH as f32) as u32;
    let y = ((0.5 - clip.y / clip.w * 0.5) * HEIGHT as f32) as u32;
    (x, y)
}

/// Mean colour, 0 to 1, of the square `radius` pixels round `(x, y)`.
fn mean(pixels: &[u8], (x, y): (u32, u32), radius: u32) -> Vec3 {
    let mut sum = Vec3::ZERO;
    let mut n = 0.0;
    for row in y - radius..y + radius {
        for column in x - radius..x + radius {
            let o = ((row * WIDTH + column) * 4) as usize;
            sum += Vec3::new(
                f32::from(pixels[o]),
                f32::from(pixels[o + 1]),
                f32::from(pixels[o + 2]),
            ) / 255.0;
            n += 1.0;
        }
    }
    sum / n
}

const MATTE: Material = Material {
    base_color: Vec3::splat(0.5),
    ..materials::VENUE_WALL
};

/// A dark hall with a red deck by a grey wall, and a white wash on the deck.
fn hall(quality: Quality, lit: bool) -> Frame {
    let mut frame = frame_with(
        VenueEnvironment::indoor(0.0).with_floor(Floor::Concrete),
        Some([20.0, 14.0, 7.0]),
        Vec3::new(3.0, -5.5, 2.2),
        Vec3::new(-1.0, 1.5, 1.2),
        quality,
        Vec::new(),
    );
    let red = Material {
        base_color: Vec3::new(0.7, 0.08, 0.06),
        ..MATTE
    };
    add_boxes(
        &mut frame,
        &[
            (Vec3::new(-1.0, 1.0, 0.5), Vec3::new(4.0, 3.0, 1.0), red),
            (Vec3::new(-1.0, 3.0, 2.0), Vec3::new(8.0, 0.2, 4.0), MATTE),
        ],
    );
    if lit {
        frame.fixture_cones.push(wash(
            Vec3::new(-1.0, 0.0, 6.0),
            Vec3::new(-1.0, 1.2, 1.0),
            Vec3::ONE,
        ));
    }
    frame
}

/// A white light on a red deck sends red onto the grey wall behind it:
/// redder there than the wall is with the bounce off, and brighter.
#[test]
fn a_lit_deck_colours_the_wall_behind_it() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = hall(Quality::High, true);
    let at = project(&frame, Vec3::new(-1.0, 2.9, 2.6));
    let mut wall = |bounce: bool, renderer: &mut Renderer| {
        frame.bounce.enabled = bounce;
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(
            &format!("hall-bounce-{}", if bounce { "on" } else { "off" }),
            &pixels,
            WIDTH,
            HEIGHT,
        );
        mean(&pixels, at, 12)
    };
    let on = wall(true, &mut renderer);
    let off = wall(false, &mut renderer);
    eprintln!("wall over the deck: bounce on {on:.3}, off {off:.3}");
    let redness = |c: Vec3| c.x / (0.5 * (c.y + c.z)).max(1e-3);
    assert!(
        on.x > off.x * 1.5 + 0.01,
        "the wall is no brighter: {on} against {off}"
    );
    assert!(redness(on) > 1.5, "the wall took no red: {on}");
}

/// A bare hall, dark but for what a test puts in it.
fn bare_hall(eye: Vec3, target: Vec3) -> Frame {
    frame_with(
        VenueEnvironment::indoor(0.0).with_floor(Floor::Concrete),
        Some([20.0, 14.0, 7.0]),
        eye,
        target,
        Quality::High,
        Vec::new(),
    )
}

/// A tube is too thin to carry discs of its own: it takes its bounce from
/// the air round it. Over a floor lit green it is green underneath, and
/// dark with the bounce off.
#[test]
fn a_tube_over_a_lit_floor_takes_its_colour() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = bare_hall(Vec3::new(0.0, -5.0, 0.6), Vec3::new(0.0, 0.0, 2.0));
    add_boxes(
        &mut frame,
        &[(Vec3::new(0.0, 0.0, 2.0), Vec3::new(6.0, 0.06, 0.06), MATTE)],
    );
    frame.fixture_cones.push(wash(
        Vec3::new(0.0, 0.5, 6.5),
        Vec3::new(0.0, 0.5, 0.0),
        Vec3::new(0.05, 1.0, 0.1),
    ));
    let at = project(&frame, Vec3::new(1.5, -0.03, 1.97));
    let mut tube = |bounce: bool, renderer: &mut Renderer| {
        frame.bounce.enabled = bounce;
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(
            &format!("tube-bounce-{}", if bounce { "on" } else { "off" }),
            &pixels,
            WIDTH,
            HEIGHT,
        );
        mean(&pixels, at, 2)
    };
    let on = tube(true, &mut renderer);
    let off = tube(false, &mut renderer);
    eprintln!("tube over the pool: bounce on {on:.3}, off {off:.3}");
    assert!(
        on.y > off.y + 0.02,
        "the tube took nothing: {on} against {off}"
    );
    assert!(on.y > 2.0 * on.x.max(on.z), "the tube is not green: {on}");
}

/// A wall stands across the floor with a pool of light on one side. The
/// floor at its foot on the far side takes none of it: no light through the
/// wall, and none round its foot.
#[test]
fn a_wall_keeps_the_light_on_its_own_side() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = bare_hall(Vec3::new(0.0, -7.0, 5.0), Vec3::new(0.0, 0.0, 0.0));
    add_boxes(
        &mut frame,
        &[(Vec3::new(0.0, 0.0, 1.5), Vec3::new(0.2, 8.0, 3.0), MATTE)],
    );
    frame.fixture_cones.push(wash(
        Vec3::new(2.0, 0.0, 6.5),
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::ONE,
    ));
    let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    capture("wall-between", &pixels, WIDTH, HEIGHT);
    // The floor a little way from the wall's foot, on each side, outside
    // the pool itself.
    let near = mean(&pixels, project(&frame, Vec3::new(0.5, -2.5, 0.0)), 4);
    let far = mean(&pixels, project(&frame, Vec3::new(-0.5, -2.5, 0.0)), 4);
    eprintln!("floor by the wall: lit side {near:.3}, far side {far:.3}");
    assert!(near.x > 0.02, "the lit side took no bounce: {near}");
    assert!(
        far.x < 0.15 * near.x,
        "light came through the wall: {far} against {near}"
    );
}

/// A light turned off takes its bounce with it: the picture after is the
/// picture of a hall that was never lit.
#[test]
fn a_light_turned_off_leaves_no_bounce_behind() {
    let mut renderer = Renderer::new().unwrap();
    let dark = renderer
        .render(&hall(Quality::High, false), WIDTH, HEIGHT, 1)
        .unwrap();
    let lit = renderer
        .render(&hall(Quality::High, true), WIDTH, HEIGHT, 1)
        .unwrap();
    let after = renderer
        .render(&hall(Quality::High, false), WIDTH, HEIGHT, 1)
        .unwrap();
    assert_ne!(dark, lit);
    assert!(dark == after, "the hall kept light after its lamp went out");
}

/// A camera outside a room looks in through the wall on its side, which is
/// not drawn. The wall is still there for the light: a wash on it lights
/// the floor at its foot.
#[test]
fn a_wall_the_camera_looks_through_still_bounces_light() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = frame_with(
        VenueEnvironment::indoor(0.0).with_floor(Floor::Concrete),
        Some([20.0, 14.0, 7.0]),
        Vec3::new(0.0, -16.0, 6.0),
        Vec3::new(0.0, -6.0, 0.0),
        Quality::High,
        Vec::new(),
    );
    assert_eq!(
        frame.room_hidden.len(),
        1,
        "the near wall is looked through"
    );
    // Painted pale, so its bounce shows.
    for draw in &mut frame.room_hidden {
        draw.material = MATTE;
    }
    frame.fixture_cones.push(wash(
        Vec3::new(0.0, -3.0, 5.0),
        Vec3::new(0.0, -7.0, 2.0),
        Vec3::new(1.0, 0.2, 0.1),
    ));
    let at = project(&frame, Vec3::new(0.0, -6.0, 0.0));
    let mut floor = |bounce: bool, renderer: &mut Renderer| {
        frame.bounce.enabled = bounce;
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(
            &format!("hidden-wall-bounce-{}", if bounce { "on" } else { "off" }),
            &pixels,
            WIDTH,
            HEIGHT,
        );
        mean(&pixels, at, 6)
    };
    let on = floor(true, &mut renderer);
    let off = floor(false, &mut renderer);
    eprintln!("floor by the hidden wall: bounce on {on:.3}, off {off:.3}");
    assert!(
        on.x > off.x + 0.02,
        "the hidden wall sent nothing: {on} against {off}"
    );
}

fn piece(id: String, geometry: Geometry, pos: [f32; 3], rot: [f32; 3]) -> Piece {
    Piece {
        id,
        geometry,
        kind: String::new(),
        pos,
        rot,
        scale: 1.0,
    }
}
fn stage() -> Vec<Piece> {
    let mut pieces = Vec::new();
    for column in 0..12 {
        for row in 0..3 {
            pieces.push(piece(
                format!("deck-{column}-{row}"),
                Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                [-6.0 + column as f32, row as f32 * 2.0, 0.0],
                [0.0; 3],
            ));
        }
    }
    for i in 0..10 {
        pieces.push(piece(
            format!("sub-{i}"),
            Geometry::mesh("stage_lab/speaker_dual18sub.glb"),
            [-5.4 + i as f32 * 1.1, -1.6, 0.0],
            [0.0; 3],
        ));
    }
    for (i, y) in [0.3, 5.7].into_iter().enumerate() {
        pieces.push(piece(
            format!("span-{i}"),
            Geometry::Procedural(Procedural::Truss { span: 12.0 }),
            [0.0, y, 7.0],
            [0.0; 3],
        ));
        for (j, x) in [-6.2, 6.2].into_iter().enumerate() {
            pieces.push(piece(
                format!("tower-{i}-{j}"),
                Geometry::Procedural(Procedural::Truss { span: 7.0 }),
                [x, y, 3.5],
                [0.0, std::f32::consts::FRAC_PI_2, 0.0],
            ));
        }
    }
    pieces
}

/// A real stage in a hall: decks, subs and a flown truss, under two
/// coloured washes. The truss and the subs are small things, lit from the
/// air: with the bounce on, the truss over the lit deck is brighter
/// underneath than with it off.
#[test]
fn a_truss_over_a_lit_deck_is_lit_from_below() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = frame_with(
        VenueEnvironment::indoor(0.0).with_floor(Floor::Concrete),
        Some([24.0, 18.0, 9.0]),
        Vec3::new(9.0, -10.0, 6.5),
        Vec3::new(0.0, 3.0, 2.5),
        Quality::High,
        stage(),
    );
    // Bright: the deck is black.
    for (x, colour) in [
        (-3.0, Vec3::new(1.0, 0.25, 0.1)),
        (3.0, Vec3::new(0.1, 0.4, 1.0)),
    ] {
        frame.fixture_cones.push(FixtureCone {
            intensity: 60.0,
            ..wash(Vec3::new(x, 3.0, 6.6), Vec3::new(x * 0.8, 3.0, 1.0), colour)
        });
    }
    let started = std::time::Instant::now();
    let mut picture = |bounce: bool, renderer: &mut Renderer| {
        frame.bounce.enabled = bounce;
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(
            &format!("stage-bounce-{}", if bounce { "on" } else { "off" }),
            &pixels,
            WIDTH,
            HEIGHT,
        );
        pixels
    };
    let on = picture(true, &mut renderer);
    eprintln!(
        "first frame with its bake: {:.2} s",
        started.elapsed().as_secs_f32()
    );
    let off = picture(false, &mut renderer);
    // The far span, over the deck between the pools.
    let at = project(&frame, Vec3::new(-2.5, 5.7, 7.0));
    let (lit, dark) = (mean(&on, at, 6), mean(&off, at, 6));
    eprintln!("truss over the deck: bounce on {lit:.3}, off {dark:.3}");
    assert!(
        lit.x > dark.x + 0.01,
        "the truss took nothing from the deck: {lit} against {dark}"
    );
}
