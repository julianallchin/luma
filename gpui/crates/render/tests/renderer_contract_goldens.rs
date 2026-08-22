//! Tracked, self-describing renderer acceptance images.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use luma_render::scene_desc::{Definition, Scene};
use luma_render::{assets, build_frame, Renderer};
use serde::Deserialize;

const CASES: &[&str] = &[
    "textured-pbr",
    "metal-roughness-sweep",
    "sun-direction-left",
    "sun-direction-right",
    "sun-off",
    "one-beam",
    "overlapping-beams",
    "occluded-beam",
    "gobo-seam-negative",
    "gobo-seam-positive",
    "volumetric-performance-smooth",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    schema: String,
    image: String,
    output_size: [u32; 2],
    subframes: u32,
    time_seconds: f32,
    scene: Scene,
    definitions: BTreeMap<String, Definition>,
}

#[test]
fn tracked_contract_frames_match_their_canonical_descriptors() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = crate_root.ancestors().nth(3).unwrap();
    let golden_root = crate_root.join("goldens/contracts");
    let mut library = assets::Library::new(repo.join("resources/meshes"));
    let mut renderer = Renderer::new().unwrap();
    let mut captured = BTreeMap::new();

    for id in CASES {
        let stem = format!("{id}-1.370");
        let descriptor: Descriptor =
            serde_json::from_reader(File::open(golden_root.join(format!("{stem}.json"))).unwrap())
                .unwrap();
        assert_eq!(descriptor.schema, "luma.renderer-frame/1");
        assert_eq!(descriptor.image, format!("{stem}.png"));
        assert_eq!(descriptor.scene.id, *id);
        assert_eq!(descriptor.time_seconds, 1.37);
        assert_eq!(descriptor.subframes, luma_render::DEFAULT_SUBFRAMES);

        let frame = build_frame(
            &descriptor.scene,
            &descriptor.definitions,
            descriptor.time_seconds,
            &mut library,
        )
        .unwrap();
        if *id == "textured-pbr" {
            assert!(frame.images.iter().any(|texture| {
                texture.image.width > 1
                    && texture.image.height > 1
                    && texture
                        .image
                        .rgba
                        .chunks_exact(4)
                        .any(|pixel| pixel != &texture.image.rgba[0..4])
            }));
        }
        let pixels = renderer
            .render(
                &frame,
                descriptor.output_size[0],
                descriptor.output_size[1],
                descriptor.subframes,
            )
            .unwrap();
        let expected = read_png(&golden_root.join(&descriptor.image));
        assert_eq!(
            pixels, expected,
            "{id} changed from its tracked deterministic capture"
        );
        captured.insert(*id, pixels);
    }

    assert_ne!(
        captured["sun-direction-left"],
        captured["sun-direction-right"]
    );
    assert!(mean_rgb(&captured["sun-off"]) < mean_rgb(&captured["sun-direction-left"]));
    assert_eq!(
        captured["gobo-seam-negative"], captured["gobo-seam-positive"],
        "equivalent angles on opposite sides of the gobo wrap must meet without a seam"
    );
}

fn read_png(path: &Path) -> Vec<u8> {
    let decoder = png::Decoder::new(File::open(path).unwrap());
    let mut reader = decoder.read_info().unwrap();
    assert_eq!(reader.info().color_type, png::ColorType::Rgba);
    assert_eq!(reader.info().bit_depth, png::BitDepth::Eight);
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    pixels.truncate(info.buffer_size());
    pixels
}

fn mean_rgb(pixels: &[u8]) -> f64 {
    pixels
        .chunks_exact(4)
        .map(|pixel| f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2]))
        .sum::<f64>()
        / (pixels.len() / 4 * 3) as f64
}
