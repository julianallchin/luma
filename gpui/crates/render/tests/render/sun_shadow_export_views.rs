//! Two exported Gasworks Park views, a few degrees of orbit apart.
//!
//! In one the stage deck cast no sun shadow over part of the ground in front
//! of it; in the other it did. The ground was one 400 km quad, and in the
//! first view the rasteriser put its pixels' world positions almost a metre
//! from where they are, so the ground read the shadow map in the wrong place.
//!
//! The files carry the camera, the sun and every caster's bounding sphere.
//! The deck, the subs and the crowd barriers are rebuilt as boxes on those
//! spheres, and each view must shadow the ground in front of the deck exactly
//! where a ray to the sun hits one of them.
use std::collections::BTreeMap;
use std::path::Path;

use glam::{Mat4, Vec3, Vec4};
use luma_render::{
    assets::{Library, Material},
    build_frame_with,
    camera_export::CameraExport,
    frame::{Draw, MaterialTextures},
    scene_desc::{CameraPose, DebugView, RenderSettings, Scene, VenueEnvironment, VenueHaze},
    Frame, Renderer,
};

/// `(centre, size)` boxes for the exported casters the test rebuilds: the
/// deck's 2x1x1 m platforms, the subs and the barriers.
fn boxes(export: &CameraExport) -> Vec<(Vec3, Vec3)> {
    export
        .casters
        .iter()
        .filter_map(|caster| {
            let centre = Vec3::from(caster.centre);
            let size = match caster.mesh.as_str() {
                "stage_lab/stage_praticavel_2x1x1.glb#0" => Vec3::new(2.0, 1.0, 1.0),
                "stage_lab/speaker_dual18sub.glb#0" => Vec3::new(1.2, 0.8, 0.56),
                "stage_lab/guardrail.glb#0" => Vec3::new(2.2, 0.05, 1.0),
                _ => return None,
            };
            Some((centre, size))
        })
        .collect()
}

fn frame(export: &CameraExport, boxes: &[(Vec3, Vec3)]) -> Frame {
    let elevation = export.sun.expect("the view has a sun").elevation_deg;
    let mut render = RenderSettings::room(
        VenueEnvironment::outdoor(elevation),
        VenueHaze::default(),
        export.camera.fov_y_deg,
        1.0,
    );
    render.haze.enabled = false;
    render.show_grid = false;
    render.show_gizmos = false;
    render.debug_view = DebugView::Shadow;
    let three = |p: [f32; 3]| [p[0], p[2], -p[1]];
    let scene = Scene {
        id: "sun-shadow-export-views".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: three(export.camera.eye),
            target: three(export.camera.target),
        },
        editing: false,
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces: Vec::new(),
        state: BTreeMap::new(),
    };
    let mut frame = build_frame_with(
        &scene,
        &BTreeMap::new(),
        &|_, _| None,
        0.0,
        &mut Library::default(),
    )
    .unwrap();
    let mesh = frame.meshes.len();
    frame.meshes.push(crate::common::cube("::export-view-cube"));
    let at = frame.draws.len() - frame.transparent.len();
    let draws: Vec<_> = boxes
        .iter()
        .map(|&(centre, size)| Draw {
            strobe: luma_render::strobe::Rows::STEADY,
            mesh,
            model: Mat4::from_translation(centre) * Mat4::from_scale(size),
            material: Material {
                base_color: Vec3::splat(0.5),
                roughness: 0.8,
                ..Default::default()
            },
            textures: MaterialTextures::default(),
            editor_object: None,
        })
        .collect();
    frame.draws.splice(at..at, draws);
    export.apply(&mut frame);
    frame
}

fn hits(boxes: &[(Vec3, Vec3)], from: Vec3, direction: Vec3, length: f32) -> bool {
    boxes.iter().any(|&(centre, size)| {
        let lo = centre - size * 0.5;
        let hi = centre + size * 0.5;
        let inv = direction.recip();
        let a = (lo - from) * inv;
        let b = (hi - from) * inv;
        let enter = a.min(b).max_element();
        let exit = a.max(b).min_element();
        enter <= exit && exit > 0.0 && enter < length
    })
}

fn check(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let export = CameraExport::read(&path).unwrap();
    let boxes = boxes(&export);
    assert!(boxes.len() > 60, "{name}: the deck, subs and barriers");
    let frame = frame(&export, &boxes);
    let (width, height) = export.render_size();
    let pixels = Renderer::new()
        .unwrap()
        .render(&frame, width, height, 1)
        .unwrap();
    let sun = Vec3::from(export.sun.unwrap().direction).normalize();
    let view_proj = export.projection() * export.view();
    let eye = Vec3::from(export.camera.eye);
    let mut failures = Vec::new();
    // The ground from the deck's front edge to past the barriers.
    for iy in 0..=40 {
        for ix in 0..=80 {
            let p = Vec3::new(-8.0 + ix as f32 * 0.2, iy as f32 * 0.1, 0.0);
            let expected: Vec<bool> = [
                Vec3::ZERO,
                Vec3::X * 0.1,
                -Vec3::X * 0.1,
                Vec3::Y * 0.1,
                -Vec3::Y * 0.1,
            ]
            .iter()
            .map(|offset| hits(&boxes, p + *offset + Vec3::Z * 0.01, sun, 100.0))
            .collect();
            if expected.iter().any(|e| *e != expected[0]) {
                continue;
            }
            let to_eye = eye - p;
            if hits(&boxes, p, to_eye.normalize(), to_eye.length()) {
                continue;
            }
            let clip = view_proj * Vec4::new(p.x, p.y, p.z, 1.0);
            let ndc = clip.truncate() / clip.w;
            let x = ((ndc.x * 0.5 + 0.5) * width as f32) as u32;
            let y = ((0.5 - ndc.y * 0.5) * height as f32) as u32;
            if x >= width || y >= height {
                continue;
            }
            let visibility = f32::from(pixels[((y * width + x) * 4) as usize]) / 255.0;
            let ok = if expected[0] {
                visibility < 0.3
            } else {
                visibility > 0.7
            };
            if !ok {
                failures.push(format!(
                    "{name}: ground {p} should be {} but has sun visibility {visibility:.2}",
                    if expected[0] { "shadowed" } else { "lit" }
                ));
            }
        }
    }
    failures
}

#[test]
fn the_deck_shadows_the_ground_in_front_of_it_in_both_exported_views() {
    // About 1500 points are checked per view. Before the ground was cut
    // into small cells, 994 were wrong in the first view and 4 in the second.
    let mut failures = check("gasworks-view-deck-unshadowed.json");
    failures.extend(check("gasworks-view-deck-shadowed.json"));
    assert!(
        failures.len() < 20,
        "{} ground points wrong:\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
