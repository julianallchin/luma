//! Generate tracked renderer-contract images and canonical input sidecars.
//!
//!     cargo run -p luma-render --release --bin render-contract-goldens

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use luma_render::coords::data_pose_of;
use luma_render::scene_desc::{
    CameraPose, DirectionalLight, Environment, Geometry, Piece, Procedural, RenderSettings, Scene,
};
use luma_render::truss::{Face, FaceSet};
use luma_render::{assets, build_frame, Catalogue, Renderer, DEFAULT_SUBFRAMES};

const TIME: f32 = 1.37;

fn main() -> anyhow::Result<()> {
    let repo = repo_root();
    let source = Catalogue::load(&repo.join("gpui/crates/render/goldens/scenes.json"))?;
    let output = repo.join("gpui/crates/render/goldens/contracts");
    std::fs::create_dir_all(&output)?;
    let mut renderer = Renderer::new()?;
    let mut library = assets::Library::new(repo.join("resources/meshes"));
    let requested = std::env::args().skip(1).collect::<Vec<_>>();

    for scene in material_scenes() {
        if !requested.is_empty() && !requested.contains(&scene.id) {
            continue;
        }
        render_case(
            &mut renderer,
            &mut library,
            &Catalogue {
                warmup_frames: DEFAULT_SUBFRAMES,
                viewport: luma_render::scene_desc::Viewport {
                    width: 640,
                    height: 400,
                },
                device_scale_factor: 1.0,
                definitions: BTreeMap::new(),
                scenes: Vec::new(),
            },
            &scene,
            &output,
        )?;
    }
    for scene in [
        shadow_scene("sun-shadow-hard", 0.0),
        shadow_scene("sun-shadow-soft", 3.0),
    ] {
        if !requested.is_empty() && !requested.contains(&scene.id) {
            continue;
        }
        render_case(
            &mut renderer,
            &mut library,
            &Catalogue {
                warmup_frames: DEFAULT_SUBFRAMES,
                viewport: luma_render::scene_desc::Viewport {
                    width: 640,
                    height: 400,
                },
                device_scale_factor: 1.0,
                definitions: BTreeMap::new(),
                scenes: Vec::new(),
            },
            &scene,
            &output,
        )?;
    }

    let beam_catalogue = Catalogue {
        warmup_frames: DEFAULT_SUBFRAMES,
        viewport: luma_render::scene_desc::Viewport {
            width: 800,
            height: 500,
        },
        device_scale_factor: 1.0,
        definitions: copy_definitions(&source)?,
        scenes: Vec::new(),
    };
    // These are *aliases* of `scenes.json` entries, not new scenes: the
    // contract name says what is being pinned, the source id says what is
    // being rendered. `one-beam` and `single-mover` are the same scene at two
    // resolutions, and so are `overlapping-beams` and `mover-fan`;
    // `par-occlusion` is rendered three times over and `single-mover` three
    // times over. Counting contract goldens and `scenes-wgpu` images together
    // therefore overstates scene coverage — a review set that lists both names
    // is claiming evidence it does not have. Confirmed by pixels: downscaled
    // 2x the pairs agree to meanAbs 0.37/255.
    for (source_id, id, edit, fixture_shadows) in [
        ("single-mover", "one-beam", None, false),
        ("mover-fan", "overlapping-beams", None, false),
        ("par-occlusion", "occluded-beam", None, false),
        ("par-occlusion", "fixture-shadow-open", None, false),
        ("par-occlusion", "fixture-shadowed-beam", None, true),
        (
            "single-mover",
            "gobo-seam-negative",
            Some((1, -0.001)),
            false,
        ),
        (
            "single-mover",
            "gobo-seam-positive",
            Some((1, std::f32::consts::TAU - 0.001)),
            false,
        ),
    ] {
        if !requested.is_empty() && !requested.iter().any(|requested| requested == id) {
            continue;
        }
        let mut scene = copy_scene(find_scene(&source, source_id)?);
        scene.id = id.into();
        scene.times = vec![TIME];
        if id.starts_with("fixture-shadow") {
            let caster = scene
                .pieces
                .first_mut()
                .expect("fixture-shadow contract keeps its caster");
            caster.pos = [-0.20, -1.2, 1.4];
            caster.scale = 0.20;
        }
        scene.render.fixture_shadows = fixture_shadows;
        if fixture_shadows {
            scene.render.fixture_surface_lighting = false;
        }
        if let Some((gobo, rotation)) = edit {
            let state = scene
                .state
                .get_mut("mover:0")
                .expect("single-mover keeps its primary head");
            state.gobo = gobo;
            state.gobo_rotation = rotation;
        }
        render_case(
            &mut renderer,
            &mut library,
            &beam_catalogue,
            &scene,
            &output,
        )?;
    }

    for scene in [truss_scene(), corner_scene(), joint_scene(), hinge_scene()] {
        if requested.is_empty() || requested.contains(&scene.id) {
            render_case(
                &mut renderer,
                &mut library,
                &Catalogue {
                    warmup_frames: DEFAULT_SUBFRAMES,
                    viewport: luma_render::scene_desc::Viewport {
                        width: 1100,
                        height: 480,
                    },
                    device_scale_factor: 1.0,
                    definitions: BTreeMap::new(),
                    scenes: Vec::new(),
                },
                &scene,
                &output,
            )?;
        }
    }

    let mut performance = copy_scene(find_scene(&source, "dense-venue")?);
    performance.id = "volumetric-performance-smooth".into();
    performance.times = vec![TIME];
    performance.render.haze.resolution = 0.5;
    let performance_catalogue = Catalogue {
        warmup_frames: DEFAULT_SUBFRAMES,
        viewport: luma_render::scene_desc::Viewport {
            width: 960,
            height: 540,
        },
        device_scale_factor: 2.0,
        definitions: copy_definitions(&source)?,
        scenes: Vec::new(),
    };
    if requested.is_empty() || requested.contains(&performance.id) {
        render_case(
            &mut renderer,
            &mut library,
            &performance_catalogue,
            &performance,
            &output,
        )?;
    }
    Ok(())
}

fn material_scenes() -> Vec<Scene> {
    let material = |id: &str, mesh_path: &str, sun: Option<[f32; 3]>| {
        let mut render = RenderSettings::dark_stage(45.0, 1.0);
        render.environment = Environment {
            background: [0.004, 0.006, 0.01],
            ambient_color: [0.35, 0.45, 0.65],
            ambient_intensity: 0.08,
            probe: None,
        };
        render.haze.enabled = false;
        render.show_grid = false;
        render.sun = sun.map(|direction| DirectionalLight {
            direction,
            color: [1.0, 0.93, 0.82],
            intensity: 2.4,
            shadows: false,
            shadow_softness: 1.0,
        });
        Scene {
            id: id.into(),
            times: vec![TIME],
            camera: CameraPose {
                position: [4.5, 3.0, 7.5],
                target: [0.0, 0.0, 0.0],
            },
            editing: false,
            render,
            selected_fixture_ids: Vec::new(),
            editor: Default::default(),
            fixtures: Vec::new(),
            pieces: vec![Piece {
                id: "lab".into(),
                geometry: Geometry::mesh(format!("../../gpui/crates/render/goldens/{mesh_path}")),
                kind: "floor".into(),
                pos: [0.0; 3],
                rot: [0.0; 3],
                scale: if mesh_path == "textured-pbr.gltf" {
                    2.2
                } else {
                    0.72
                },
            }],
            state: BTreeMap::new(),
        }
    };
    vec![
        material("textured-pbr", "textured-pbr.gltf", Some([2.0, -3.0, 6.0])),
        material(
            "metal-roughness-sweep",
            "pbr-sweep.gltf",
            Some([2.0, -3.0, 6.0]),
        ),
        material(
            "sun-direction-left",
            "pbr-sweep.gltf",
            Some([-5.0, -2.0, 4.0]),
        ),
        material(
            "sun-direction-right",
            "pbr-sweep.gltf",
            Some([5.0, -2.0, 4.0]),
        ),
        material("sun-off", "pbr-sweep.gltf", None),
    ]
}

/// The stage the whole truss family is read against: one hard sun, no haze, no
/// grid, so the only thing in the picture is tube.
fn truss_bench(id: &str, camera: CameraPose, pieces: Vec<Piece>) -> Scene {
    let mut render = RenderSettings::dark_stage(38.0, 1.0);
    render.environment = Environment {
        background: [0.004, 0.006, 0.01],
        ambient_color: [0.35, 0.45, 0.65],
        ambient_intensity: 0.10,
        probe: None,
    };
    render.haze.enabled = false;
    render.show_grid = false;
    render.sun = Some(DirectionalLight {
        direction: [2.0, -2.5, 6.0],
        color: [1.0, 0.95, 0.88],
        intensity: 2.6,
        shadows: false,
        shadow_softness: 1.0,
    });
    Scene {
        id: id.into(),
        times: vec![TIME],
        camera,
        editing: false,
        editor: Default::default(),
        render,
        selected_fixture_ids: Vec::new(),
        fixtures: Vec::new(),
        pieces,
        state: BTreeMap::new(),
    }
}

/// A generated truss piece, posed in three space rather than stored space.
///
/// The family's local space *is* three space, so a piece's pose is the same
/// matrix its geometry is built in — which is what lets [`bolted`] hand a
/// mating transform straight to [`data_pose_of`].
fn generated(id: &str, geometry: Procedural, pose: glam::Mat4) -> Piece {
    let (pos, rot) = data_pose_of(pose);
    Piece {
        id: id.into(),
        geometry: Geometry::Procedural(geometry),
        kind: "truss".into(),
        pos,
        rot,
        scale: 1.0,
    }
}

/// `guest` bolted onto the `nth` open face of a host posed at `host_pose`.
///
/// Every joint in the joint golden is built this way, through
/// `EndFrame::mating` and nothing else — so a generator whose end frames drift
/// out of register does not quietly render straight, it renders a visibly
/// broken run. That is the entire point of the picture.
fn bolted(
    id: &str,
    host: Procedural,
    host_pose: glam::Mat4,
    nth: usize,
    guest: Procedural,
) -> Piece {
    let face = host.end_frames()[nth].transformed(host_pose);
    let pose = face.mating(guest.end_frames()[0]);
    generated(id, guest, pose)
}

/// The ripped SketchUp F33 stick over the procedural F34 lattice, same span,
/// same camera. The picture the procedural generator is read against.
fn truss_scene() -> Scene {
    truss_bench(
        "truss-side-by-side",
        CameraPose {
            position: [0.0, 0.95, 2.7],
            target: [0.0, 0.95, 0.0],
        },
        vec![
            Piece {
                id: "reference".into(),
                geometry: Geometry::mesh("stage_lab/truss_q30_1.22m.glb"),
                kind: "truss".into(),
                pos: [0.0, 0.0, 1.4],
                rot: [0.0; 3],
                scale: 1.0,
            },
            // The same stick asked for, snapped to 1.0 m: the pair that pins
            // chord gauge and brace pitch against a measured reference.
            Piece {
                id: "procedural-stick".into(),
                geometry: Geometry::Procedural(Procedural::Truss { span: 1.2192 }),
                kind: "truss".into(),
                pos: [0.0, 0.0, 0.95],
                rot: [0.0; 3],
                scale: 1.0,
            },
            // A real run, which is the point of generating rather than
            // importing: one continuous lattice, not three sticks in a row.
            Piece {
                id: "procedural-run".into(),
                geometry: Geometry::Procedural(Procedural::Truss { span: 3.0 }),
                kind: "truss".into(),
                pos: [0.0, 0.0, 0.4],
                rot: [0.0; 3],
                scale: 1.0,
            },
        ],
    )
}

/// The ripped Q30 corner block beside the generated ones, left to right: the
/// reference, a two-way L, and a six-way.
///
/// The reference is modelled with its origin on a corner of its own bounding
/// box rather than at its centre, so it is offset by half a block to put the
/// three on one axis — the mismatch a generated family does not have.
fn corner_scene() -> Scene {
    const RIPPED_HALF: f32 = 0.1524;
    truss_bench(
        "truss-corner-vs-ripped",
        CameraPose {
            position: [0.0, 1.25, 1.85],
            target: [0.0, 0.9, 0.0],
        },
        vec![
            Piece {
                id: "reference".into(),
                geometry: Geometry::mesh("stage_lab/truss_q30_box.glb"),
                kind: "truss".into(),
                // Stored pose is data space: `(x, y, z)` with `z` up.
                pos: [-0.62 - RIPPED_HALF, RIPPED_HALF, 0.9 - RIPPED_HALF],
                rot: [0.0; 3],
                scale: 1.0,
            },
            generated(
                "corner-2-way",
                Procedural::Corner {
                    faces: FaceSet::of([Face::NegX, Face::PosY]),
                },
                glam::Mat4::from_translation(glam::Vec3::new(0.0, 0.9, 0.0)),
            ),
            generated(
                "corner-6-way",
                Procedural::Corner {
                    faces: FaceSet::ALL,
                },
                glam::Mat4::from_translation(glam::Vec3::new(0.62, 0.9, 0.0)),
            ),
        ],
    )
}

/// Three joints with a stick bolted to every open face: an L, a T, and a hinge
/// at 45°.
///
/// Every stick's pose comes from `EndFrame::mating` against the block's own
/// face, so any disagreement between a stick's end and a block's face — a
/// half-square that is not half a square, a roll a quarter turn out — shows up
/// as a kinked run rather than as a number in a test nobody reads.
fn joint_scene() -> Scene {
    const STICK: Procedural = Procedural::Truss { span: 0.5 };
    let mut pieces = Vec::new();
    // Yaw is composition, not decoration: a 45° hinge seen end-on is two
    // foreshortened stubs, so the joint is turned to straddle the view and the
    // legs — which are placed by mating, not by this angle — splay evenly.
    let joints: [(&str, Procedural, f32, f32); 3] = [
        (
            "l",
            Procedural::Corner {
                faces: FaceSet::of([Face::PosX, Face::PosY]),
            },
            -2.0,
            0.0,
        ),
        (
            "t",
            Procedural::Corner {
                faces: FaceSet::of([Face::NegX, Face::PosX, Face::PosY]),
            },
            0.0,
            0.0,
        ),
        ("hinge", Procedural::Hinge { angle: 45.0 }, 2.0, -22.5),
    ];
    for (name, joint, x, yaw) in joints {
        let pose = glam::Mat4::from_translation(glam::Vec3::new(x, 0.85, 0.0))
            * glam::Mat4::from_rotation_y(yaw.to_radians());
        pieces.push(generated(name, joint, pose));
        for (nth, _) in joint.end_frames().iter().enumerate() {
            pieces.push(bolted(
                &format!("{name}-leg-{nth}"),
                joint,
                pose,
                nth,
                STICK,
            ));
        }
    }
    truss_bench(
        "truss-joints",
        CameraPose {
            position: [0.0, 3.0, 3.5],
            target: [0.0, 0.75, 0.0],
        },
        pieces,
    )
}

/// The same hinge at four deflections, from above.
///
/// A hinge is the one piece in the family whose geometry is a continuum, so
/// one angle proves nothing: the leaves have to clear each other, stay on the
/// pin, and read as hardware at every one of them. Seen from above because the
/// deflection is a yaw, and a yaw seen edge-on is two foreshortened stubs.
fn hinge_scene() -> Scene {
    let pieces = [0.0f32, 45.0, 90.0, 135.0]
        .into_iter()
        .enumerate()
        .map(|(nth, angle)| {
            generated(
                &format!("hinge-{angle:.0}"),
                Procedural::Hinge { angle },
                glam::Mat4::from_translation(glam::Vec3::new(nth as f32 * 0.86 - 1.29, 0.9, 0.0)),
            )
        })
        .collect();
    truss_bench(
        "truss-hinge-angles",
        CameraPose {
            position: [0.0, 1.68, 1.95],
            target: [0.0, 0.9, 0.0],
        },
        pieces,
    )
}

fn shadow_scene(id: &str, shadow_softness: f32) -> Scene {
    let mut render = RenderSettings::dark_stage(42.0, 1.0);
    render.environment = Environment {
        background: [0.004, 0.006, 0.01],
        ambient_color: [0.3, 0.38, 0.5],
        ambient_intensity: 0.12,
        probe: None,
    };
    render.haze.enabled = false;
    render.show_grid = false;
    render.sun = Some(DirectionalLight {
        direction: [-5.0, -3.0, 8.0],
        color: [1.0, 0.93, 0.82],
        intensity: 2.8,
        shadows: true,
        shadow_softness,
    });
    Scene {
        id: id.into(),
        times: vec![TIME],
        camera: CameraPose {
            position: [6.5, 4.0, 8.5],
            target: [0.0, 0.8, 0.0],
        },
        editing: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces: vec![
            Piece {
                id: "receiver".into(),
                geometry: Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                kind: "floor".into(),
                pos: [0.0, 0.0, 0.0],
                rot: [0.0; 3],
                scale: 2.4,
            },
            Piece {
                id: "caster".into(),
                geometry: Geometry::mesh("stage_lab/speaker_dbr15.glb"),
                kind: "speaker".into(),
                pos: [0.0, 1.1, 0.0],
                rot: [0.0, 0.35, 0.0],
                scale: 1.2,
            },
        ],
        state: BTreeMap::new(),
    }
}

fn render_case(
    renderer: &mut Renderer,
    library: &mut assets::Library,
    catalogue: &Catalogue,
    scene: &Scene,
    output: &Path,
) -> anyhow::Result<()> {
    let (width, height) = catalogue.frame_size();
    let frame = build_frame(scene, &catalogue.definitions, TIME, library)?;
    // This synchronous render includes the fixed-seed subframe accumulation,
    // GPU completion and readback: it is the capture-ready barrier.
    let pixels = renderer.render(&frame, width, height, DEFAULT_SUBFRAMES)?;
    let image = output.join(scene.frame_name(TIME));
    luma_render::image_out::write(&image, &pixels, width, height)?;
    let descriptor = catalogue.frame_descriptor(scene, TIME, DEFAULT_SUBFRAMES)?;
    write_json(&output.join(scene.descriptor_name(TIME)), &descriptor)?;
    println!(
        "{}  {} draws, {} cones, {}x{}",
        image.display(),
        frame.draws.len(),
        frame.fixture_cones.len(),
        width,
        height
    );
    Ok(())
}

fn find_scene<'a>(catalogue: &'a Catalogue, id: &str) -> anyhow::Result<&'a Scene> {
    catalogue
        .scenes
        .iter()
        .find(|scene| scene.id == id)
        .ok_or_else(|| anyhow::anyhow!("missing source scene {id}"))
}

fn copy_scene(scene: &Scene) -> Scene {
    serde_json::from_value(serde_json::to_value(scene).expect("scene serializes"))
        .expect("canonical scene round trips")
}

fn copy_definitions(
    catalogue: &Catalogue,
) -> anyhow::Result<BTreeMap<String, luma_render::scene_desc::Definition>> {
    Ok(serde_json::from_value(serde_json::to_value(
        &catalogue.definitions,
    )?)?)
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)?;
    std::io::Write::write_all(&mut writer, b"\n")?;
    Ok(())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate is three levels below the repo root")
        .to_path_buf()
}
