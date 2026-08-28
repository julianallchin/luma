//! Luma's mark, drawn rather than loaded.
//!
//! The app icon is an **L made of light**: a fan of beams leaving one bright
//! point, the longest reaching up for the stem and the widest lying right for
//! the foot. That description is the whole drawing, so the mark is a handful of
//! triangles sharing an apex — which is why it is painted here instead of
//! shipped as an asset. A raster icon would need an asset source registered in
//! three binaries and would still be the wrong thing: the shipped icon is a
//! coloured squircle, and a screen wants the bare mark on its own ground.
//!
//! Monochrome by construction. The beams differ in *alpha*, not in hue, so the
//! mark reads on any plane the ladder can produce.

use gpui::{point, px, AnyElement, IntoElement, ParentElement as _, PathBuilder, Styled as _};

/// One beam: where it points, how far it reaches, and how wide it opens —
/// polar because that is what a fan is. Lengths and angles are fractions of the
/// mark's box, so the whole figure scales with one number.
struct Beam {
    /// Degrees clockwise from the +x axis (y grows downward), so negative is up.
    angle: f32,
    /// Reach from the apex, as a fraction of the box's edge.
    length: f32,
    /// Half-opening, in degrees.
    spread: f32,
    alpha: f32,
}

/// Where every beam starts, as a fraction of the box. Low and left: the corner
/// of the L is the light, and everything else is what it throws.
const APEX: (f32, f32) = (0.14, 0.84);

/// The figure. The stem and the foot carry the letter; the three between them
/// are the fan that makes it light rather than type.
const BEAMS: &[Beam] = &[
    Beam {
        angle: -74.0,
        length: 0.84,
        spread: 4.2,
        alpha: 1.00,
    },
    Beam {
        angle: -56.0,
        length: 0.42,
        spread: 2.8,
        alpha: 0.34,
    },
    Beam {
        angle: -38.0,
        length: 0.52,
        spread: 2.6,
        alpha: 0.28,
    },
    Beam {
        angle: -18.0,
        length: 0.44,
        spread: 2.4,
        alpha: 0.24,
    },
    Beam {
        angle: 0.0,
        length: 0.86,
        spread: 4.8,
        alpha: 0.95,
    },
];

/// The mark, `edge` pixels square.
///
/// Painted into a canvas of that size; it claims no more room than the square
/// and draws nothing outside it.
#[must_use]
pub fn luma(edge: f32) -> AnyElement {
    gpui::div()
        .size(px(edge))
        .flex_none()
        .child(gpui::canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let scale = f32::from(bounds.size.width);
                let apex = point(
                    bounds.origin.x + px(APEX.0 * scale),
                    bounds.origin.y + px(APEX.1 * scale),
                );
                for beam in BEAMS {
                    let mut path = PathBuilder::fill();
                    path.move_to(apex);
                    for edge_angle in [beam.angle - beam.spread, beam.angle + beam.spread] {
                        let (sin, cos) = edge_angle.to_radians().sin_cos();
                        path.line_to(point(
                            apex.x + px(cos * beam.length * scale),
                            apex.y + px(sin * beam.length * scale),
                        ));
                    }
                    path.close();
                    if let Ok(path) = path.build() {
                        window.paint_path(path, gpui::white().opacity(beam.alpha));
                    }
                }
            },
        ))
        .into_any_element()
}
