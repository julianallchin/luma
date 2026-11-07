//! Helpers more than one file in this suite needs.

use std::path::{Path, PathBuf};

use glam::Vec3;
use luma_render::assets::Library;
use luma_scene::mesh::{MeshData, Vertex};

/// A mesh library over the repository's meshes.
pub fn library() -> Library {
    Library::new(luma_render::meshes_root())
}

/// With `LUMA_RENDER_CAPTURE_DIR` set, saves an RGBA8 picture a test looked
/// at as `<dir>/<test file>/<name>.png`, so a run can be inspected by eye.
/// Otherwise does nothing.
#[track_caller]
pub fn capture(name: &str, pixels: &[u8], width: u32, height: u32) {
    let Some(dir) = std::env::var_os("LUMA_RENDER_CAPTURE_DIR") else {
        return;
    };
    let file = Path::new(std::panic::Location::caller().file())
        .file_stem()
        .expect("a test file has a name");
    let dir = PathBuf::from(dir).join(file);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    luma_render::image_out::write(&dir.join(format!("{name}.png")), pixels, width, height)
        .unwrap_or_else(|e| panic!("{name}: {e}"));
}

/// A unit cube centred on the origin, as a mesh named `key`.
pub fn cube(key: &str) -> MeshData {
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
        key: key.into(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
}

/// The mean of the R, G and B channels of an RGBA8 image, over every pixel.
pub fn mean_rgb(pixels: &[u8]) -> f64 {
    pixels
        .chunks_exact(4)
        .map(|pixel| f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2]))
        .sum::<f64>()
        / (pixels.len() / 4 * 3) as f64
}
