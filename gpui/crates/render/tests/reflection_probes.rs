//! Local reflection probes (`probes.rs`): shiny and matte things take their
//! surroundings from the stage around them, relit every frame.
//!
//! `LUMA_PROBE_CAPTURE_DIR` saves every frame these tests look at, with the
//! probes on and off. The timing test is ignored by default; run it with
//! `LUMA_PROFILE_DETAIL=1` so the renderer records each pass.
use std::{collections::BTreeMap, path::PathBuf};

use glam::{Mat4, Vec3};
use luma_render::{
    Frame, Renderer,
    assets::{Library, Material, Vertex},
    build_frame_with,
    frame::{Draw, FixtureCone, MaterialTextures, MeshData},
    luminaire::Lens,
    materials,
    scene_desc::{
        CameraPose, CloudCover, Floor, Geometry, Piece, Procedural, Quality, RenderSettings, Scene,
        VenueEnvironment, VenueHaze,
    },
};

const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;

fn library() -> Library {
    Library::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"))
}

/// A frame of `pieces` in `environment`, from `eye` toward `target` (world
/// metres, Z up), with no haze and no editor overlays.
fn frame_with(
    environment: VenueEnvironment,
    eye: Vec3,
    target: Vec3,
    quality: Quality,
    pieces: Vec<Piece>,
) -> Frame {
    let mut render = RenderSettings::room(environment, VenueHaze::default(), 50.0, 1.0);
    render.haze.enabled = false;
    render.show_grid = false;
    render.show_gizmos = false;
    render.quality = quality;
    // Camera poses are three-space: (x, up, -y).
    let three = |p: Vec3| [p.x, p.z, -p.y];
    let scene = Scene {
        id: "reflection-probes".into(),
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

fn outdoor(floor: Floor) -> VenueEnvironment {
    VenueEnvironment::outdoor(35.0)
        .with_sun_azimuth(200.0)
        .with_clouds(CloudCover::Overcast)
        .with_floor(floor)
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
        wash: 1.0,
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
        lens: Lens::POINT,
        strobe: luma_render::strobe::Rows::STEADY,
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
        key: "::probe-test-cube".into(),
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
                strobe: luma_render::strobe::Rows::STEADY,
            },
        );
    }
}

/// Render `frame` until the probes hold it, then once more: the picture a
/// viewer sees once the capture has gone round.
fn settled(renderer: &mut Renderer, frame: &Frame) -> Vec<u8> {
    for _ in 0..80 {
        renderer.render(frame, WIDTH, HEIGHT, 1).unwrap();
        if renderer.reflection_probes_pending() == 0 {
            break;
        }
    }
    assert_eq!(
        renderer.reflection_probes_pending(),
        0,
        "the probes never settled"
    );
    renderer.render(frame, WIDTH, HEIGHT, 1).unwrap()
}

fn capture(name: &str, pixels: &[u8]) {
    if let Some(dir) = std::env::var_os("LUMA_PROBE_CAPTURE_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        luma_render::image_out::write(&dir.join(format!("{name}.png")), pixels, WIDTH, HEIGHT)
            .unwrap();
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

/// The stage of `images/33.png`: a 12 by 6 m deck a metre high, a row of
/// subs in front, a flown truss rectangle on four towers.
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

/// A grey galvanised barrier on dirt takes the dirt's warmth into its
/// reflection, against the same barrier on concrete. The camera stands
/// above it, so its face reflects the ground between the two. Crowd
/// barriers on warm dirt were a cool grey pasted on the ground
/// (`images/34.png`).
#[test]
fn a_galvanised_barrier_on_dirt_reflects_the_warm_ground() {
    let mut renderer = Renderer::new().unwrap();
    let eye = Vec3::new(0.0, -4.0, 1.7);
    let centre = Vec3::new(0.0, 0.0, 0.55);
    let warmth = |floor: Floor, probes: bool, renderer: &mut Renderer| {
        let mut frame = frame_with(outdoor(floor), eye, centre, Quality::High, Vec::new());
        frame.probes.enabled = probes;
        add_boxes(
            &mut frame,
            &[(centre, Vec3::new(2.4, 0.08, 1.1), materials::STEEL)],
        );
        let pixels = settled(renderer, &frame);
        capture(
            &format!(
                "barrier-{}-probes-{}",
                floor.set(),
                if probes { "on" } else { "off" }
            ),
            &pixels,
        );
        let face = mean(&pixels, project(&frame, centre - Vec3::Y * 0.05), 10);
        // Red over blue: the dirt is warm, the concrete near neutral.
        face.x / face.z.max(1e-3)
    };
    let dirt = warmth(Floor::Dirt, true, &mut renderer);
    let concrete = warmth(Floor::Concrete, true, &mut renderer);
    let dirt_off = warmth(Floor::Dirt, false, &mut renderer);
    let concrete_off = warmth(Floor::Concrete, false, &mut renderer);
    eprintln!(
        "barrier face red over blue: dirt {dirt:.3}, concrete {concrete:.3} \
         (sky probe only: dirt {dirt_off:.3}, concrete {concrete_off:.3})"
    );

    // The user's view (`images/34.png`): barriers and subs on dirt in front
    // of the stage, from a little above head height. For the pictures only.
    let mut pieces = stage();
    for i in 0..8 {
        pieces.push(piece(
            format!("barrier-{i}"),
            Geometry::mesh("stage_lab/guardrail.glb"),
            [-9.0 + 2.5 * i as f32, 5.0, 0.0],
            [0.0; 3],
        ));
    }
    let mut frame = frame_with(
        outdoor(Floor::Dirt),
        Vec3::new(-2.0, -15.0, 3.0),
        Vec3::new(1.0, -3.0, 0.5),
        Quality::High,
        pieces,
    );
    for probes in [false, true] {
        frame.probes.enabled = probes;
        capture(
            &format!("barriers-view-probes-{}", if probes { "on" } else { "off" }),
            &settled(&mut renderer, &frame),
        );
    }

    assert!(
        dirt > concrete + 0.05,
        "the barrier is no warmer on dirt: {dirt:.3} against {concrete:.3}"
    );
}

/// A red pool on the deck shows in the truss over it in the frame the
/// fixture turns red. The fixture lights the deck and not the truss, so
/// the only way the red reaches the truss is its reflection of the deck.
/// Only the truss's own pixels count: the same view without the span
/// finds them.
#[test]
fn a_truss_reflects_a_red_pool_in_the_frame_it_turns_red() {
    let mut renderer = Renderer::new().unwrap();
    let truss = Vec3::new(0.0, 0.3, 7.0);
    let pool = Vec3::new(0.0, 1.5, 1.0);
    let frame = |color: Vec3, span: bool, probes: bool| {
        let pieces = stage()
            .into_iter()
            .filter(|piece| span || piece.id != "span-0")
            .collect();
        let mut frame = frame_with(
            outdoor(Floor::Concrete),
            Vec3::new(1.5, -2.0, 4.2),
            truss,
            Quality::High,
            pieces,
        );
        frame.probes.enabled = probes;
        for x in [-3.0, 3.0] {
            frame
                .fixture_cones
                .push(wash(Vec3::new(x, -2.0, 4.0), pool, color * 4.0));
        }
        frame
    };
    // The truss's pixels: where the view changes when the span goes.
    let with_span = settled(&mut renderer, &frame(Vec3::ONE, true, false));
    let without_span = settled(&mut renderer, &frame(Vec3::ONE, false, false));
    let mask: Vec<usize> = (0..(WIDTH * HEIGHT) as usize)
        .filter(|&i| (0..3).any(|c| with_span[i * 4 + c].abs_diff(without_span[i * 4 + c]) > 12))
        .collect();
    assert!(mask.len() > 2000, "the span covers {} pixels", mask.len());
    let red_over_green = |pixels: &[u8]| {
        mask.iter()
            .map(|&i| (f32::from(pixels[i * 4]) - f32::from(pixels[i * 4 + 1])) / 255.0)
            .sum::<f32>()
            / mask.len() as f32
    };
    let shift = |probes: bool, renderer: &mut Renderer| {
        let white = frame(Vec3::ONE, true, probes);
        let red = frame(Vec3::new(1.0, 0.03, 0.03), true, probes);
        let before = settled(renderer, &white);
        // One frame: the probes' captures are unchanged, their relight is not.
        let after = renderer.render(&red, WIDTH, HEIGHT, 1).unwrap();
        let tag = if probes { "on" } else { "off" };
        capture(&format!("truss-white-probes-{tag}"), &before);
        capture(&format!("truss-red-probes-{tag}"), &after);
        red_over_green(&after) - red_over_green(&before)
    };
    let with = shift(true, &mut renderer);
    let without = shift(false, &mut renderer);
    // The rig under a magenta wash, from the deck's front edge. For the
    // pictures only.
    let mut wide = frame_with(
        outdoor(Floor::Concrete),
        Vec3::new(2.5, -3.5, 2.5),
        Vec3::new(-1.0, 0.3, 6.0),
        Quality::High,
        stage(),
    );
    for x in [-4.5, -1.5, 1.5, 4.5] {
        wide.fixture_cones.push(wash(
            Vec3::new(x, -3.0, 5.0),
            Vec3::new(x, 1.5, 1.0),
            Vec3::new(1.0, 0.1, 0.7) * 4.0,
        ));
    }
    for probes in [false, true] {
        wide.probes.enabled = probes;
        capture(
            &format!("truss-wash-probes-{}", if probes { "on" } else { "off" }),
            &settled(&mut renderer, &wide),
        );
    }
    eprintln!(
        "truss red shift over {} truss pixels in the frame the pool turns red: {with:.4} \
         (sky probe only {without:.4})",
        mask.len()
    );
    assert!(
        with > 0.03 && with > 5.0 * without.abs(),
        "the truss does not take the red pool: {with:.4} against {without:.4} without probes"
    );
}

/// Indoors, a shiny box reflects the room's walls: its mirror face is
/// green in a green room and not in a grey one. The room has no sky, so
/// without the probes the face reflects nothing at all.
#[test]
fn indoors_a_shiny_box_reflects_the_rooms_walls() {
    let mut renderer = Renderer::new().unwrap();
    let centre = Vec3::new(0.0, 0.0, 1.5);
    let chrome = Material {
        base_color: Vec3::splat(0.9),
        metallic: 1.0,
        roughness: 0.12,
        ..Default::default()
    };
    let room = |wall_color: Vec3| {
        let wall = Material {
            base_color: wall_color,
            roughness: 0.9,
            ..Default::default()
        };
        let mut frame = frame_with(
            VenueEnvironment::indoor(1.0).with_floor(Floor::Concrete),
            Vec3::new(0.0, -3.5, 1.6),
            centre,
            Quality::High,
            Vec::new(),
        );
        add_boxes(
            &mut frame,
            &[
                (Vec3::new(0.0, 6.0, 2.5), Vec3::new(16.0, 0.2, 5.0), wall),
                (Vec3::new(0.0, -6.0, 2.5), Vec3::new(16.0, 0.2, 5.0), wall),
                (Vec3::new(8.0, 0.0, 2.5), Vec3::new(0.2, 12.0, 5.0), wall),
                (Vec3::new(-8.0, 0.0, 2.5), Vec3::new(0.2, 12.0, 5.0), wall),
                (centre, Vec3::splat(1.0), chrome),
            ],
        );
        // Soft light on the walls, so they have something to show and
        // their colour does not clip to white.
        for x in [-6.0, 0.0, 6.0] {
            for y in [-5.0, 5.0] {
                frame.fixture_cones.push(wash(
                    Vec3::new(x, y * 0.5, 4.8),
                    Vec3::new(x, y, 1.5),
                    Vec3::splat(0.3),
                ));
            }
        }
        frame
    };
    let mut green_room = room(Vec3::new(0.08, 0.6, 0.1));
    let mut grey_room = room(Vec3::splat(0.35));
    let at = project(&green_room, centre - Vec3::Y * 0.5);
    let face = |frame: &mut Frame, name: &str, probes: bool, renderer: &mut Renderer| {
        frame.probes.enabled = probes;
        let pixels = settled(renderer, frame);
        capture(
            &format!("indoor-{name}-probes-{}", if probes { "on" } else { "off" }),
            &pixels,
        );
        mean(&pixels, at, 20)
    };
    let greenness = |c: Vec3| c.y / (0.5 * (c.x + c.z)).max(1e-3);
    let green = face(&mut green_room, "green", true, &mut renderer);
    let grey = face(&mut grey_room, "grey", true, &mut renderer);
    let dark = face(&mut green_room, "green", false, &mut renderer);
    eprintln!(
        "mirror face: green room {green:.3} (green over red and blue {:.3}), grey room {grey:.3} \
         ({:.3}), green room without probes {dark:.3}",
        greenness(green),
        greenness(grey),
    );
    assert!(
        greenness(green) > greenness(grey) + 0.3,
        "the box does not take the room's green: {:.3} against {:.3} in a grey room",
        greenness(green),
        greenness(grey)
    );
    assert!(
        green.length() > 3.0 * dark.length() + 0.05,
        "the box reflects little more with the probes: {green:.3} against {dark:.3}"
    );
}

/// The ground under a deck stays dark with the probes on. The probes stand
/// over the deck and see open sky and lit ground; a point under it that
/// took their light was as bright as open ground (`images/36.png`). The
/// under-deck ground's brightness over open ground's must be no higher with
/// the probes than without.
#[test]
fn the_ground_under_a_deck_stays_dark_with_probes() {
    let mut renderer = Renderer::new().unwrap();
    let deck = Material {
        base_color: Vec3::splat(0.05),
        roughness: 0.7,
        ..Default::default()
    };
    let under = Vec3::new(0.0, 3.0, 0.0);
    let open = Vec3::new(0.0, -2.5, 0.0);
    let ratio = |probes: bool, renderer: &mut Renderer| {
        let mut frame = frame_with(
            outdoor(Floor::Dirt),
            Vec3::new(0.5, -7.0, 0.7),
            Vec3::new(0.0, 3.0, 0.3),
            Quality::High,
            Vec::new(),
        );
        frame.probes.enabled = probes;
        let mut boxes = vec![(Vec3::new(0.0, 3.0, 1.0), Vec3::new(10.0, 5.0, 0.1), deck)];
        for x in [-4.9, 4.9] {
            for y in [0.6, 5.4] {
                boxes.push((
                    Vec3::new(x, y, 0.5),
                    Vec3::new(0.1, 0.1, 1.0),
                    materials::STEEL,
                ));
            }
        }
        add_boxes(&mut frame, &boxes);
        let pixels = settled(renderer, &frame);
        capture(
            &format!("under-deck-probes-{}", if probes { "on" } else { "off" }),
            &pixels,
        );
        let luminance = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
        luminance(mean(&pixels, project(&frame, under), 6))
            / luminance(mean(&pixels, project(&frame, open), 6)).max(1e-4)
    };
    let off = ratio(false, &mut renderer);
    let on = ratio(true, &mut renderer);
    eprintln!("under-deck over open ground: probes on {on:.3}, off {off:.3}");

    // The user's view (`images/35.png`, `36.png`): the deck's front and its
    // legs over dirt, barriers in front. For the pictures only.
    let mut pieces = stage();
    for i in 0..8 {
        pieces.push(piece(
            format!("barrier-{i}"),
            Geometry::mesh("stage_lab/guardrail.glb"),
            [-9.0 + 2.5 * i as f32, 3.5, 0.0],
            [0.0; 3],
        ));
    }
    let mut frame = frame_with(
        outdoor(Floor::Dirt),
        Vec3::new(-7.0, -11.0, 1.7),
        Vec3::new(1.0, -1.0, 0.6),
        Quality::High,
        pieces,
    );
    for probes in [false, true] {
        frame.probes.enabled = probes;
        capture(
            &format!("deck-view-probes-{}", if probes { "on" } else { "off" }),
            &settled(&mut renderer, &frame),
        );
    }
    assert!(
        on <= off + 0.02,
        "the ground under the deck is brighter with the probes: {on:.3} against {off:.3}"
    );
}

/// Open ground takes the same light inside the probe grid as past it, at a
/// low sun and a high one: no lit or warm disk round the stage where the
/// grid ends. The probes' own estimate of the open sky and ground was not
/// the sky probe's, so the grid showed as an ellipse on the ground ("looks
/// like a nuke went off"): at a 2 degree sun the ground just inside was 11%
/// brighter over the ground past it than without the probes, and redder.
/// The ground under the deck stays darker than open ground.
///
/// The deck is 8 by 4 m, so the grid (its bounds and 2 m) ends 4 m in front
/// of its middle and fades out over a cell of 3 m. Inside is 3.6 m in front,
/// outside 8 m. Each point's colour with the probes on is compared with its
/// colour without them, which takes the view, the stage's shadow and the
/// height field's occlusion out of the comparison: what is left is the
/// probes' change. A little of it is real: the rough ground reflects the
/// deck, which the probes, standing over it, see lit from above.
#[test]
fn open_ground_has_no_edge_at_the_probe_grid() {
    let mut renderer = Renderer::new().unwrap();
    let deck = Material {
        base_color: Vec3::splat(0.3),
        roughness: 0.7,
        ..Default::default()
    };
    let inside = Vec3::new(0.0, -3.6, 0.0);
    let outside = Vec3::new(0.0, -8.0, 0.0);
    let under = Vec3::new(0.0, 0.0, 0.0);
    let luminance = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
    // Red and blue over luminance: the warmth the edge showed as.
    let chroma = |c: Vec3| Vec3::new(c.x, 0.0, c.z) / luminance(c).max(1e-4);
    for elevation in [2.0, 55.0] {
        let colours = |probes: bool, renderer: &mut Renderer| {
            let environment = VenueEnvironment::outdoor(elevation)
                .with_sun_azimuth(200.0)
                .with_floor(Floor::Dirt);
            let mut frame = frame_with(
                environment,
                Vec3::new(0.0, -15.0, 3.0),
                Vec3::new(0.0, -2.0, 0.0),
                Quality::High,
                Vec::new(),
            );
            frame.probes.enabled = probes;
            let mut boxes = vec![(Vec3::new(0.0, 0.0, 1.0), Vec3::new(8.0, 4.0, 0.1), deck)];
            for x in [-3.9, 3.9] {
                for y in [-1.9, 1.9] {
                    boxes.push((
                        Vec3::new(x, y, 0.5),
                        Vec3::new(0.1, 0.1, 1.0),
                        materials::STEEL,
                    ));
                }
            }
            add_boxes(&mut frame, &boxes);
            let pixels = settled(renderer, &frame);
            capture(
                &format!(
                    "grid-edge-sun-{elevation}-probes-{}",
                    if probes { "on" } else { "off" }
                ),
                &pixels,
            );
            [inside, outside, under].map(|p| mean(&pixels, project(&frame, p), 6))
        };
        let [in_off, out_off, under_off] = colours(false, &mut renderer);
        let [in_on, out_on, under_on] = colours(true, &mut renderer);
        let step = |inside: Vec3, outside: Vec3| luminance(inside) / luminance(outside).max(1e-4);
        // The step at the grid's edge with the probes, over the same step
        // without them: 1 when the grid does not show.
        let edge = step(in_on, out_on) / step(in_off, out_off);
        let warmth = (chroma(in_on) - chroma(out_on) - (chroma(in_off) - chroma(out_off))).length();
        let under_ratio = step(under_on, out_on);
        eprintln!(
            "sun {elevation} deg: edge {edge:.3}, colour shift {warmth:.4}; under the deck \
             over open ground {under_ratio:.3} (without probes {:.3}); inside on {in_on:.3} \
             off {in_off:.3}",
            step(under_off, out_off)
        );
        assert!(
            (edge - 1.0).abs() < 0.04,
            "sun {elevation} deg: the grid's edge shows on open ground: the step across it is \
             {edge:.3} of the step without the probes"
        );
        assert!(
            warmth < 0.03,
            "sun {elevation} deg: the grid's edge changes open ground's colour by {warmth:.4}"
        );
        assert!(
            under_ratio < 0.8,
            "sun {elevation} deg: the ground under the deck is not darker than open ground: \
             {under_ratio:.3} of it"
        );
    }
}

/// At dusk the truss towers are no brighter in a band at the horizon than
/// what they reflect there. A vertical tube at eye height reflects the
/// horizon: the sky just above it and the ground just below. The probes had
/// that ground lit and without the air the camera sees it through, and the
/// sky probe's below-horizon glow past it, so every tower carried a bright
/// tan band across the horizon (`images/37.png`). Only the towers' own
/// pixels count: the same view without them finds them.
#[test]
fn the_towers_carry_no_bright_band_at_the_horizon() {
    let mut renderer = Renderer::new().unwrap();
    let environment = VenueEnvironment::outdoor(-2.0)
        .with_sun_azimuth(200.0)
        .with_floor(Floor::Dirt);
    let eye = Vec3::new(1.0, -9.0, 1.7);
    let target = Vec3::new(0.0, 3.0, 1.7);
    let view = |towers: bool, probes: bool| {
        let pieces = stage()
            .into_iter()
            .filter(|piece| towers || !piece.id.starts_with("tower-"))
            .collect();
        let mut frame = frame_with(environment, eye, target, Quality::High, pieces);
        frame.probes.enabled = probes;
        frame
    };
    let without_towers = settled(&mut renderer, &view(false, false));
    let with_towers = settled(&mut renderer, &view(true, false));
    // The horizon's row: the camera looks level, so it is the middle.
    let horizon = HEIGHT / 2;
    let band = horizon..horizon + HEIGHT / 10;
    let tower: Vec<usize> = band
        .clone()
        .flat_map(|row| (0..WIDTH).map(move |x| (row * WIDTH + x) as usize))
        .filter(|&i| (0..3).any(|c| with_towers[i * 4 + c].abs_diff(without_towers[i * 4 + c]) > 8))
        .collect();
    assert!(
        tower.len() > 500,
        "the towers cover {} band pixels",
        tower.len()
    );
    let luminance = |pixels: &[u8], i: usize| {
        0.2126 * f32::from(pixels[i * 4]) / 255.0
            + 0.7152 * f32::from(pixels[i * 4 + 1]) / 255.0
            + 0.0722 * f32::from(pixels[i * 4 + 2]) / 255.0
    };
    let mean = |pixels: &[u8], set: &[usize]| {
        set.iter().map(|&i| luminance(pixels, i)).sum::<f32>() / set.len() as f32
    };
    // What a level reflection there can see: the rows just above and just
    // below the horizon, away from the towers.
    let rows = |range: std::ops::Range<u32>| -> Vec<usize> {
        range
            .flat_map(|row| (0..WIDTH).map(move |x| (row * WIDTH + x) as usize))
            .filter(|&i| {
                (0..3).all(|c| with_towers[i * 4 + c].abs_diff(without_towers[i * 4 + c]) <= 8)
            })
            .collect()
    };
    let sky = mean(&without_towers, &rows(horizon - 20..horizon - 4));
    let ground = mean(&without_towers, &rows(horizon + 4..horizon + 20));
    let off = mean(&with_towers, &tower);
    let probes = settled(&mut renderer, &view(true, true));
    let on = mean(&probes, &tower);
    capture("horizon-towers-probes-off", &with_towers);
    capture("horizon-towers-probes-on", &probes);
    eprintln!(
        "tower band at the horizon: probes on {on:.3}, off {off:.3}; \
         sky above {sky:.3}, ground below {ground:.3}"
    );
    assert!(
        on <= sky.max(ground) * 1.1 + 0.01,
        "the towers' band outshines what they reflect: {on:.3} against sky {sky:.3} and ground {ground:.3}"
    );
    assert!(
        on <= off * 1.25 + 0.01,
        "the probes brighten the towers' band: {on:.3} against {off:.3}"
    );
}

/// A shiny box moved across the probe grid in half-metre steps changes its
/// reflection smoothly: no step is much larger than the others, as it would
/// be where it crosses from one probe's share to the next.
#[test]
fn a_reflection_changes_smoothly_across_the_probe_grid() {
    let mut renderer = Renderer::new().unwrap();
    let chrome = Material {
        base_color: Vec3::splat(0.9),
        metallic: 1.0,
        roughness: 0.2,
        ..Default::default()
    };
    // Subs along the back to span the grid, and coloured pools on the ground
    // where the box's face reflects it, so its reflection has something to
    // change through.
    let pieces = || -> Vec<Piece> {
        (0..16)
            .map(|i| {
                piece(
                    format!("sub-{i}"),
                    Geometry::mesh("stage_lab/speaker_dual18sub.glb"),
                    [-9.0 + 1.2 * i as f32, 4.0, 0.0],
                    [0.0; 3],
                )
            })
            .collect()
    };
    let colors = [
        Vec3::new(1.0, 0.1, 0.1),
        Vec3::new(1.0, 0.8, 0.1),
        Vec3::new(0.1, 1.0, 0.1),
        Vec3::new(0.1, 0.8, 1.0),
        Vec3::new(0.2, 0.1, 1.0),
    ];
    let mut colours = Vec::new();
    for step in 0..=24 {
        let x = -6.0 + 0.5 * step as f32;
        let centre = Vec3::new(x, 0.0, 0.8);
        let mut frame = frame_with(
            outdoor(Floor::Concrete),
            centre + Vec3::new(0.0, -3.0, 0.4),
            centre,
            Quality::High,
            pieces(),
        );
        for (i, color) in colors.iter().enumerate() {
            let pool = Vec3::new(-8.0 + 4.0 * i as f32, -5.0, 0.0);
            frame
                .fixture_cones
                .push(wash(pool + Vec3::new(0.0, 1.0, 6.0), pool, *color * 3.0));
        }
        add_boxes(&mut frame, &[(centre, Vec3::splat(0.4), chrome)]);
        let pixels = settled(&mut renderer, &frame);
        if step % 6 == 0 {
            capture(&format!("seams-{step:02}"), &pixels);
        }
        colours.push(mean(&pixels, project(&frame, centre - Vec3::Y * 0.2), 8));
    }
    let steps: Vec<f32> = colours.windows(2).map(|w| (w[1] - w[0]).length()).collect();
    let mut sorted = steps.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    let largest = sorted[sorted.len() - 1];
    eprintln!(
        "reflection change per half-metre step: median {median:.4}, largest {largest:.4}; \
         colours {colours:.3?}"
    );
    assert!(
        largest < 3.0 * median + 0.02,
        "a step in the reflection: {largest:.4} against a median of {median:.4}: {steps:.4?}"
    );
}

/// The probes' GPU cost on a real rig at 1920x1080 with the camera moving:
/// the relight and the prefilter every frame, the sampling as the scene
/// pass with the probes against without, and the capture while it runs.
/// A report, not a bound: GPU time on a shared machine is not a contract.
#[test]
#[ignore = "timing report; LUMA_PROFILE_DETAIL=1 adds the per-pass split"]
fn reflection_probe_timings() {
    // Per-pass brackets put timestamps between the passes, and those cost
    // time of their own: the headline delta is measured without them.
    let detail = std::env::var_os("LUMA_PROFILE_DETAIL").is_some_and(|v| v == "1");
    let mut renderer = Renderer::new_profiled().unwrap();
    let rig = |frame: &mut Frame| {
        // 48 heads on the two spans, in a spread of colours, aimed across
        // the deck and the ground in front of it.
        for (i, y) in [0.3_f32, 5.7].into_iter().enumerate() {
            for k in 0..24 {
                let x = -5.75 + 0.5 * k as f32;
                let hue = (k as f32 / 24.0 + i as f32 * 0.5) * std::f32::consts::TAU;
                let color = Vec3::new(
                    0.5 + 0.5 * hue.cos(),
                    0.5 + 0.5 * (hue + 2.1).cos(),
                    0.5 + 0.5 * (hue + 4.2).cos(),
                );
                let aim = Vec3::new(x * 1.4, -3.0 + 6.0 * i as f32, 0.0);
                frame
                    .fixture_cones
                    .push(wash(Vec3::new(x, y, 6.8), aim, color * 2.0));
            }
        }
    };
    let span = |timings: &luma_render::FrameTimings, name: &str| {
        timings
            .passes
            .iter()
            .filter(|pass| pass.name == name)
            .map(|pass| pass.end_ms - pass.start_ms)
            .sum::<f64>()
    };
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let qualities: Vec<Quality> = match std::env::var("LUMA_PROBE_TIMING").as_deref() {
        Ok("low") => vec![Quality::Low],
        Ok("high") => vec![Quality::High],
        _ => vec![Quality::High, Quality::Low],
    };
    for quality in qualities {
        let at = |step: usize, probes: bool| {
            let turn = step as f32 * 0.03;
            let eye = Vec3::new(18.0 * turn.sin(), -18.0 * turn.cos(), 3.0);
            let mut frame = frame_with(
                outdoor(Floor::Dirt),
                eye,
                Vec3::new(0.0, 2.0, 2.0),
                quality,
                stage(),
            );
            rig(&mut frame);
            frame.probes.enabled = probes;
            frame
        };
        // The capture, from a fresh placement: every frame until it is done.
        let mut capture = Vec::new();
        let mut step = 0;
        loop {
            let timings = renderer
                .profile_live_frame(&at(step, true), 1920, 1080, 1)
                .unwrap();
            step += 1;
            capture.push(span(&timings, "probe-capture"));
            if renderer.reflection_probes_pending() == 0 || step > 80 {
                break;
            }
        }
        // On and off alternate, each pair at one camera position and in
        // alternating order, so a GPU clock that drifts over a run (a light
        // Low frame lets it) moves both sides alike. Two blocks of frames,
        // one on and one off, put Low's delta anywhere from 0.13 to 0.72 ms.
        let (mut relight, mut filter, mut deltas, mut total_on, mut total_off) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let pairs = std::env::var("LUMA_PROBE_PAIRS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(104_usize);
        for i in 0..pairs {
            let mut pair = [0.0; 2];
            for k in 0..2 {
                let probes = (k == 0) == (i % 2 == 0);
                let timings = renderer
                    .profile_live_frame(&at(step + i, probes), 1920, 1080, 1)
                    .unwrap();
                pair[usize::from(!probes)] = timings.gpu_total_ms;
                if probes && i >= 8 {
                    relight.push(span(&timings, "probe-relight"));
                    filter.push(span(&timings, "probe-filter"));
                }
            }
            if i >= 8 {
                deltas.push(pair[0] - pair[1]);
                total_on.push(pair[0]);
                total_off.push(pair[1]);
            }
        }
        let frames = capture.len();
        let capture_max = capture.iter().copied().fold(0.0, f64::max);
        let capture_median = median(&mut capture);
        let (relight, filter) = (median(&mut relight), median(&mut filter));
        let delta = median(&mut deltas);
        // The mean of the middle half: steadier than the median alone
        // between runs, as the GPU's clock wanders.
        let middle = &deltas[deltas.len() / 4..deltas.len() * 3 / 4];
        let trimmed = middle.iter().sum::<f64>() / middle.len() as f64;
        let (with, without) = (median(&mut total_on), median(&mut total_off));
        eprintln!(
            "{quality:?} 1920x1080, 48 heads, camera moving: WHOLE-FRAME DELTA {delta:.3} ms \
             (median of {} paired frames, middle-half mean {trimmed:.3}; {with:.2} ms with, \
             {without:.2} ms without)",
            deltas.len()
        );
        if !detail {
            eprintln!("{quality:?} passes: run with LUMA_PROFILE_DETAIL=1 for the split");
            continue;
        }
        eprintln!(
            "{quality:?} passes: relight {relight:.3} ms, prefilter {filter:.3} ms, \
             sampling about {:.3} ms (delta less both); capture {capture_median:.3} ms a frame \
             (max {capture_max:.3}) over {frames} frames",
            delta - relight - filter
        );
    }
}

/// Diagnosis: dump the probe nearest the barrier on dirt: its captured
/// albedo, normal and distance, and its change to the sky probe (black
/// where negative) at every prefiltered mip, to `LUMA_PROBE_CAPTURE_DIR`. Run with `LUMA_PROBE_DEBUG=1` for the
/// balls too.
#[test]
#[ignore = "diagnosis"]
fn dump_probe_textures() {
    let mut renderer = Renderer::new().unwrap();
    let eye = Vec3::new(0.0, -4.0, 1.7);
    let centre = Vec3::new(0.0, 0.0, 0.55);
    let mut frame = frame_with(outdoor(Floor::Dirt), eye, centre, Quality::High, Vec::new());
    add_boxes(
        &mut frame,
        &[(centre, Vec3::new(2.4, 0.08, 1.1), materials::STEEL)],
    );
    let pixels = settled(&mut renderer, &frame);
    capture("dump-view", &pixels);
    // The stage with a coloured wash, for the debug balls.
    let mut stage_view = frame_with(
        outdoor(Floor::Dirt),
        Vec3::new(-4.0, -14.0, 3.0),
        Vec3::new(0.0, 2.0, 2.0),
        Quality::High,
        stage(),
    );
    for x in [-4.0, 0.0, 4.0] {
        stage_view.fixture_cones.push(wash(
            Vec3::new(x, 0.3, 6.8),
            Vec3::new(x, 2.0, 1.0),
            Vec3::new(0.2, 0.4, 1.0) * 4.0,
        ));
    }
    capture("dump-stage", &settled(&mut renderer, &stage_view));
    let _ = settled(&mut renderer, &frame);
    let positions = renderer.reflection_probe_positions();
    eprintln!("probes: {positions:.2?}");
    let nearest = positions
        .iter()
        .enumerate()
        .min_by(|a, b| {
            (*a.1 - centre)
                .length()
                .total_cmp(&(*b.1 - centre).length())
        })
        .unwrap()
        .0 as u32;
    let dir = std::env::var_os("LUMA_PROBE_CAPTURE_DIR").map(PathBuf::from);
    let save = |name: &str,
                (w, h, texels): (u32, u32, Vec<[f32; 4]>),
                scale: f32,
                map: &dyn Fn([f32; 4]) -> [f32; 3]| {
        let mut sum = [0.0f64; 4];
        for t in &texels {
            for c in 0..4 {
                sum[c] += f64::from(t[c]);
            }
        }
        let n = texels.len() as f64;
        eprintln!(
            "{name}: {w}x{h} mean {:.4} {:.4} {:.4} {:.4}",
            sum[0] / n,
            sum[1] / n,
            sum[2] / n,
            sum[3] / n
        );
        let Some(dir) = &dir else { return };
        std::fs::create_dir_all(dir).unwrap();
        let rgba: Vec<u8> = texels
            .iter()
            .flat_map(|t| {
                let c = map(*t);
                let e = |v: f32| ((v * scale).clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8;
                [e(c[0]), e(c[1]), e(c[2]), 255]
            })
            .collect();
        luma_render::image_out::write(&dir.join(format!("{name}.png")), &rgba, w, h).unwrap();
    };
    let rgb = |t: [f32; 4]| [t[0], t[1], t[2]];
    for what in 0..3 {
        let (w, h, texels) = renderer.read_reflection_probe(nearest, what, 0).unwrap();
        let size = h;
        for face in 0..6u32 {
            let at = |u: u32, v: u32| texels[(v * w + face * size + u) as usize];
            eprintln!(
                "what {what} face {face}: centre {:.3?} quarter {:.3?} three-quarter {:.3?}",
                at(size / 2, size / 2),
                at(size / 2, size / 4),
                at(size / 2, 3 * size / 4)
            );
        }
    }
    save(
        "dump-albedo",
        renderer.read_reflection_probe(nearest, 1, 0).unwrap(),
        1.0,
        &rgb,
    );
    save(
        "dump-distance",
        renderer.read_reflection_probe(nearest, 2, 0).unwrap(),
        1.0,
        &|t| [t[3] / 20.0, t[3] / 20.0, t[3] / 20.0],
    );
    let scale = std::env::var("PROBE_DUMP_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    for mip in 0..7 {
        let Ok(read) = renderer.read_reflection_probe(nearest, 0, mip) else {
            break;
        };
        save(&format!("dump-radiance-mip{mip}"), read, scale, &rgb);
    }
}
