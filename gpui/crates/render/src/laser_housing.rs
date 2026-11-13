//! A laser projector's body, built from its definition's dimensions: the
//! chassis, the aperture in the middle of its front face behind its glass,
//! the warning labels beside it, a handle on top, vents down the sides and
//! feet underneath.
//!
//! Every projector is a box with an aperture (`docs/design/lasers.md`), so
//! there is no mesh to bundle: the parts are boxes, a tube and two
//! triangles, sized from the box. The beam leaves the middle of the glass,
//! [`Definition::aperture_m`] ahead of the origin, which is where the solver
//! puts the scan's origin.

use glam::{Mat4, Quat, Vec3};
use luma_scene::definition::Definition;
use luma_scene::mesh::{Material, MeshData, Vertex};

use crate::frame::{box_mesh, Bank, Draw, EditorObject, MaterialTextures};
use crate::materials::{ALUMINIUM, POWDER_COAT, RUBBER, STEEL};

/// The aperture's glass: dark and glossy, so it shows the room.
const GLASS: Material = Material {
    base_color: Vec3::new(0.008, 0.012, 0.014),
    roughness: 0.06,
    ..POWDER_COAT
};

/// The yellow of a laser warning label, as a printed vinyl sticker.
const LABEL: Material = Material {
    base_color: Vec3::new(0.93, 0.62, 0.01),
    roughness: 0.55,
    ..POWDER_COAT
};

/// The label's black print.
const PRINT: Material = Material {
    base_color: Vec3::splat(0.012),
    roughness: 0.6,
    ..POWDER_COAT
};

/// The parts of a projector at `base`, the mount frame every housing hangs
/// off. Box space is width along X, height along Y and depth along Z, with
/// the front face, the aperture, at +Z.
pub(crate) fn draws(
    def: &Definition,
    base: Mat4,
    bank: &mut Bank,
    editor_object: Option<&EditorObject>,
) -> Vec<Draw> {
    let [w, h, d] = def.dimensions_m();
    // The quarter turn puts the box's depth along the mount axis: its front
    // face on the mount normal, its back face on the clamp.
    let body = base * Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2);
    // A projector is hung either way up, and the mount does not say which:
    // it is drawn with its handle on top. Rolling it about its beam leaves
    // the aperture where it is.
    let body = if body.transform_vector3(Vec3::Y).z < -1e-3 {
        body * Mat4::from_rotation_z(std::f32::consts::PI)
    } else {
        body
    };
    let mut out = Vec::new();
    let mut part = |mesh: Mesh, at: Vec3, turn: Quat, material: Material| {
        let index = bank.insert(mesh.key(), || mesh.build());
        out.push(Draw {
            mesh: index,
            textures: MaterialTextures::default(),
            model: body * Mat4::from_rotation_translation(turn, at),
            material,
            editor_object: editor_object.cloned(),
        });
    };
    let flat = Quat::IDENTITY;
    let front = d / 2.0;

    part(Mesh::Box(Vec3::new(w, h, d)), Vec3::ZERO, flat, POWDER_COAT);

    // The aperture: a frame proud of the face, the glass proud of the frame.
    let (aw, ah) = (w * 0.34, h * 0.28);
    part(
        Mesh::Box(Vec3::new(aw + 0.016, ah + 0.016, 0.004)),
        Vec3::new(0.0, 0.0, front + 0.002),
        flat,
        ALUMINIUM,
    );
    part(
        Mesh::Box(Vec3::new(aw, ah, 0.002)),
        Vec3::new(0.0, 0.0, front + 0.005),
        flat,
        GLASS,
    );

    // The warning triangle, up and to the side of the aperture: black, a
    // yellow one inside it, and the black dot the starburst is at a distance.
    let side = w.min(h) * 0.3;
    let sign = Vec3::new(w / 2.0 - side * 0.75, h / 2.0 - side * 0.62, front);
    part(
        Mesh::Triangle(side, 0.0006),
        sign + Vec3::Z * 0.0003,
        flat,
        PRINT,
    );
    part(
        Mesh::Triangle(side * 0.74, 0.0006),
        sign + Vec3::new(0.0, -side * 0.012, 0.0009),
        flat,
        LABEL,
    );
    part(
        Mesh::Box(Vec3::new(side * 0.1, side * 0.1, 0.0006)),
        sign + Vec3::new(0.0, -side * 0.06, 0.0015),
        Quat::from_rotation_z(std::f32::consts::FRAC_PI_4),
        PRINT,
    );
    // The aperture label under the glass: yellow, ruled in black.
    let label = Vec3::new(0.0, -(ah / 2.0 + 0.008 + h * 0.09), front);
    part(
        Mesh::Box(Vec3::new(aw, h * 0.09, 0.0006)),
        label + Vec3::Z * 0.0003,
        flat,
        LABEL,
    );
    for rule in [-1.0, 1.0] {
        part(
            Mesh::Box(Vec3::new(aw * 0.86, h * 0.012, 0.0006)),
            label + Vec3::new(0.0, rule * h * 0.022, 0.0009),
            flat,
            PRINT,
        );
    }

    // The handle: two posts and a grip along the depth.
    let (post, rise, span) = (0.014, h * 0.3, d * 0.5);
    for end in [-1.0, 1.0] {
        part(
            Mesh::Box(Vec3::new(post, rise, post)),
            Vec3::new(0.0, h / 2.0 + rise / 2.0, end * span / 2.0),
            flat,
            ALUMINIUM,
        );
    }
    part(
        Mesh::Tube(0.011, span + post),
        Vec3::new(0.0, h / 2.0 + rise, 0.0),
        flat,
        RUBBER,
    );

    // Vents down each side, toward the back where the diodes are cooled.
    for side in [-1.0, 1.0] {
        for slat in 0..6 {
            let y = (slat as f32 - 2.5) * h * 0.1;
            part(
                Mesh::Box(Vec3::new(0.002, h * 0.045, d * 0.42)),
                Vec3::new(side * (w / 2.0 + 0.001), y, -d * 0.14),
                flat,
                STEEL,
            );
        }
    }

    // Feet.
    for (x, z) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        part(
            Mesh::Box(Vec3::new(0.03, 0.01, 0.03)),
            Vec3::new(x * w * 0.38, -h / 2.0 - 0.005, z * d * 0.38),
            flat,
            RUBBER,
        );
    }
    out
}

/// A part's shape. Sizes are in tenths of a millimetre in the key, so two
/// projectors of one size share their meshes.
#[derive(Clone, Copy)]
enum Mesh {
    Box(Vec3),
    /// Radius and length, along Z.
    Tube(f32, f32),
    /// An upright equilateral triangle in XY: its side, and how thick.
    Triangle(f32, f32),
}

impl Mesh {
    fn key(self) -> String {
        let n = |metres: f32| (metres * 1e4).round() as i64;
        match self {
            Mesh::Box(size) => format!("::laser-box:{}:{}:{}", n(size.x), n(size.y), n(size.z)),
            Mesh::Tube(radius, length) => format!("::laser-tube:{}:{}", n(radius), n(length)),
            Mesh::Triangle(side, thick) => format!("::laser-triangle:{}:{}", n(side), n(thick)),
        }
    }

    fn build(self) -> MeshData {
        match self {
            Mesh::Box(size) => box_mesh(size),
            Mesh::Tube(radius, length) => tube(radius, length),
            Mesh::Triangle(side, thick) => triangle(side, thick),
        }
    }
}

fn mesh(vertices: Vec<Vertex>, indices: Vec<u32>) -> MeshData {
    MeshData {
        key: String::new(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
}

fn vertex(position: Vec3, normal: Vec3, tangent: Vec3) -> Vertex {
    Vertex {
        position: position.to_array(),
        normal: normal.to_array(),
        uv: [0.0, 0.0],
        tangent: tangent.extend(1.0).to_array(),
    }
}

/// A capped tube along Z, smooth round its side.
fn tube(radius: f32, length: f32) -> MeshData {
    const SIDES: u32 = 20;
    let half = length / 2.0;
    let rim = |i: u32| {
        let (sin, cos) = (i as f32 / SIDES as f32 * std::f32::consts::TAU).sin_cos();
        Vec3::new(cos, sin, 0.0)
    };
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for i in 0..=SIDES {
        let out = rim(i);
        for z in [-half, half] {
            vertices.push(vertex(out * radius + Vec3::Z * z, out, Vec3::Z));
        }
    }
    for i in 0..SIDES {
        let a = i * 2;
        indices.extend([a, a + 2, a + 3, a, a + 3, a + 1]);
    }
    for (z, normal) in [(-half, -Vec3::Z), (half, Vec3::Z)] {
        let centre = vertices.len() as u32;
        vertices.push(vertex(Vec3::Z * z, normal, Vec3::X));
        for i in 0..=SIDES {
            vertices.push(vertex(rim(i) * radius + Vec3::Z * z, normal, Vec3::X));
        }
        for i in 0..SIDES {
            let (a, b) = (centre + 1 + i, centre + 2 + i);
            // Wound so each cap faces out.
            if normal.z > 0.0 {
                indices.extend([centre, a, b]);
            } else {
                indices.extend([centre, b, a]);
            }
        }
    }
    mesh(vertices, indices)
}

/// An upright equilateral triangle of `side` in XY, centred on its middle,
/// `thick` along Z: a sign's plate.
fn triangle(side: f32, thick: f32) -> MeshData {
    let height = side * 3f32.sqrt() / 2.0;
    let corners = [
        Vec3::new(-side / 2.0, -height / 3.0, 0.0),
        Vec3::new(side / 2.0, -height / 3.0, 0.0),
        Vec3::new(0.0, height * 2.0 / 3.0, 0.0),
    ];
    let half = Vec3::Z * (thick / 2.0);
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (offset, normal) in [(half, Vec3::Z), (-half, -Vec3::Z)] {
        let base = vertices.len() as u32;
        vertices.extend(corners.map(|corner| vertex(corner + offset, normal, Vec3::X)));
        if normal.z > 0.0 {
            indices.extend([base, base + 1, base + 2]);
        } else {
            indices.extend([base, base + 2, base + 1]);
        }
    }
    for i in 0..3 {
        let (from, to) = (corners[i], corners[(i + 1) % 3]);
        let along = (to - from).normalize();
        let normal = along.cross(Vec3::Z);
        let base = vertices.len() as u32;
        for corner in [from - half, to - half, to + half, from + half] {
            vertices.push(vertex(corner, normal, along));
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    mesh(vertices, indices)
}
