//! Generate tracked renderer-contract images and canonical input sidecars.
//!
//!     cargo run -p luma-render --release --bin render-contract-goldens

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use luma_render::scene_desc::{
    CameraPose, DirectionalLight, Environment, Piece, RenderSettings, Scene,
};
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
    for (source_id, id, edit) in [
        ("single-mover", "one-beam", None),
        ("mover-fan", "overlapping-beams", None),
        ("par-occlusion", "occluded-beam", None),
        ("single-mover", "gobo-seam-negative", Some((1, -0.001))),
        (
            "single-mover",
            "gobo-seam-positive",
            Some((1, std::f32::consts::TAU - 0.001)),
        ),
    ] {
        if !requested.is_empty() && !requested.iter().any(|requested| requested == id) {
            continue;
        }
        let mut scene = copy_scene(find_scene(&source, source_id)?);
        scene.id = id.into();
        scene.times = vec![TIME];
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
            fixtures: Vec::new(),
            pieces: vec![Piece {
                id: "lab".into(),
                mesh_path: format!("../../gpui/crates/render/goldens/{mesh_path}"),
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
        fixtures: Vec::new(),
        pieces: vec![
            Piece {
                id: "receiver".into(),
                mesh_path: "stage_lab/stage_praticavel_2x1x1.glb".into(),
                pos: [0.0, 0.0, 0.0],
                rot: [0.0; 3],
                scale: 2.4,
            },
            Piece {
                id: "caster".into(),
                mesh_path: "stage_lab/speaker_dbr15.glb".into(),
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
    write_png(&image, &pixels, width, height)?;
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

fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
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
