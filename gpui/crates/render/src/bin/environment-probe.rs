//! Renders one rigged venue under the atmosphere, at several sun elevations
//! and from several views, as PNGs.
//!
//! The sky has no golden: it is a picture, and the only test that matters is
//! looking at it. This is the instrument for that — `render-goldens` covers the
//! tracked contract, which the sky is deliberately absent from.
//!
//! ```text
//! cargo run -p luma-render --bin sky-probe -- <output-dir>
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use luma_render::assets::Library;
use luma_render::scene_desc::{
    CameraPose, Geometry, Piece, Procedural, RenderSettings, Scene, SkyParams,
};
use luma_scene::{Camera, Framing, View, Viewfinder};

const WIDTH: u32 = 960;
const HEIGHT: u32 = 600;

fn piece(id: &str, kind: &str, geometry: Geometry, pos: [f32; 3], rot: [f32; 3]) -> Piece {
    Piece {
        id: id.into(),
        geometry,
        kind: kind.into(),
        pos,
        rot,
        scale: 1.0,
    }
}

/// A goalpost rig with a deck under it and a stack either side: enough
/// silhouette to tell whether the sky is behind the rig and whether the sun is
/// raking it.
fn pieces() -> Vec<Piece> {
    let tower = |id: &str, x: f32| {
        piece(
            id,
            "truss",
            Geometry::Procedural(Procedural::Truss { span: 7.0 }),
            [x, 0.0, 3.5],
            [0.0, std::f32::consts::FRAC_PI_2, 0.0],
        )
    };
    let mut out = vec![
        tower("tower-left", -5.0),
        tower("tower-right", 5.0),
        piece(
            "top",
            "truss",
            Geometry::Procedural(Procedural::Truss { span: 10.0 }),
            [0.0, 0.0, 7.0],
            [0.0, 0.0, 0.0],
        ),
    ];
    for (index, x) in [-3.0_f32, -1.0, 1.0, 3.0].into_iter().enumerate() {
        out.push(piece(
            &format!("deck-{index}"),
            "deck",
            Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
            [x, 0.0, 0.0],
            [0.0, 0.0, 0.0],
        ));
    }
    for (index, x) in [-6.5_f32, 6.5].into_iter().enumerate() {
        out.push(piece(
            &format!("stack-{index}"),
            "speaker",
            Geometry::mesh("stage_lab/speaker_jbl_vtx_v20.glb"),
            [x, -1.0, 0.0],
            [0.0, 0.0, 0.0],
        ));
    }
    out
}

fn main() -> anyhow::Result<()> {
    let out_dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "sky-probe".into()),
    );
    std::fs::create_dir_all(&out_dir)?;
    let meshes = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    let mut renderer = luma_render::Renderer::new()?;
    let definitions = BTreeMap::new();

    for (label, elevation) in [
        ("noon30", 30.0_f32),
        ("dusk04", 4.0),
        ("set005", 0.5),
        ("twilight-04", -4.0),
    ] {
        for view in [View::Front, View::QuarterLeft, View::Audience] {
            let mut render = RenderSettings::dark_stage(45.0, 1.0);
            render.show_grid = false;
            render.haze.enabled = false;
            render.sky = Some(SkyParams::outdoor(elevation));

            let mut scene = Scene {
                id: format!("sky-{label}"),
                times: vec![0.0],
                camera: CameraPose {
                    position: [0.0, 0.0, 1.0],
                    target: [0.0, 0.0, 0.0],
                },
                editing: false,
                aim_arrows: false,
                render,
                selected_fixture_ids: Vec::new(),
                editor: luma_render::scene_desc::Editor::default(),
                fixtures: Vec::new(),
                pieces: pieces(),
                state: BTreeMap::new(),
            };
            let framing: Framing = scene.framing(&definitions);
            let finder = Viewfinder::new(scene.render.fov, WIDTH as f32 / HEIGHT as f32);
            let camera: Camera = Camera::for_view(view, &framing, None, &finder);
            let eye = camera.position();
            let target = camera.target;
            scene.camera = CameraPose {
                position: luma_render::coords::three_from_world(eye).into(),
                target: luma_render::coords::three_from_world(target).into(),
            };

            let frame = luma_render::build_frame_with(
                &scene,
                &definitions,
                &|_, _| None,
                0.0,
                &mut library,
            )?;
            let rgba = renderer.render(&frame, WIDTH, HEIGHT, 4)?;
            let path = out_dir.join(format!("{label}-{}.png", view.name()));
            luma_render::image_out::write(&path, &rgba, WIDTH, HEIGHT)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}
