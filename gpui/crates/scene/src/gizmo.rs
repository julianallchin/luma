//! Pure transform-gizmo hit testing and drag math.

use crate::Ray;
use glam::{Quat, Vec3};

const AXIS_LENGTH: f32 = 1.0;
const AXIS_RADIUS: f32 = 0.08;
const PLANE_OFFSET: f32 = 0.22;
const PLANE_RADIUS: f32 = 0.16;
const SCREEN_RADIUS: f32 = 0.13;
/// Radius of a rotate ring, in gizmo-local units. Public because the drawer
/// (`luma_render::overlay`) sizes its rings from it: a ring that is picked at
/// one radius and painted at another is a handle nobody can hit.
pub const RING_RADIUS: f32 = 0.8;
const RING_WIDTH: f32 = 0.08;
const PARALLEL_EPSILON: f32 = 1e-5;

/// `TransformControls.size`, as `unified-transform.tsx` passes it.
const GIZMO_SIZE: f32 = 0.5;

/// Which side of the pivot a handle is drawn on: three's gizmo mirrors every
/// arm toward the viewer, so the picker has to mirror with it.
fn side(towards_eye: f32) -> f32 {
    if towards_eye < 0.0 {
        -1.0
    } else {
        1.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    #[must_use]
    pub const fn vector(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GizmoMode {
    #[default]
    Translate,
    Rotate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PivotMode {
    Individual,
    Group,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GizmoHandle {
    TranslateAxis(Axis),
    /// Axis normal to the translation plane.
    TranslatePlane(Axis),
    TranslateScreen,
    RotateAxis(Axis),
    RotateScreen,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GizmoHit {
    pub handle: GizmoHandle,
    pub ray_t: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragFrame {
    pub plane_point: Vec3,
    pub plane_normal: Vec3,
    pub grab_offset: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragTarget {
    pub start_position: Vec3,
    pub start_rotation: Quat,
    pub anchor: Vec3,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GizmoState {
    Idle,
    Hover(GizmoHandle),
    Dragging {
        handle: GizmoHandle,
        frame: DragFrame,
        targets: Vec<DragTarget>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformTarget {
    pub position: Vec3,
    pub rotation: Quat,
    /// Authored pivot for individual rotation, such as a stage piece's bottom
    /// centre attachment point.
    pub anchor: Vec3,
}

/// The world scale that keeps the widget the same size on screen.
///
/// `TransformControls.updateMatrixWorld`: the distance to the camera times a
/// field-of-view term, capped, times `size / 7`. Spelled once because the
/// drawer and the hit test must agree to the last decimal — a widget drawn at
/// one scale and picked at another is off by a fraction of itself everywhere
/// except dead centre.
#[must_use]
pub fn gizmo_scale(distance: f32, fov_y_deg: f32) -> f32 {
    distance * (1.9 * (fov_y_deg.to_radians() / 2.0).tan()).min(7.0) * GIZMO_SIZE / 7.0
}

/// Pick visible transform handles analytically. `scale` is the gizmo's
/// constant-screen-size world scale; `view_direction` points pivot→camera.
#[must_use]
pub fn hit_test_gizmo(
    ray: Ray,
    pivot: Vec3,
    scale: f32,
    view_direction: Vec3,
    mode: GizmoMode,
) -> Option<GizmoHit> {
    let scale = scale.abs().max(f32::EPSILON);
    let view = view_direction.normalize_or(Vec3::Y);
    let mut hits = Vec::new();
    match mode {
        GizmoMode::Translate => {
            for axis in [Axis::X, Axis::Y, Axis::Z] {
                let vector = axis.vector();
                // The arm the viewer sees, not the one the axis names: an arm
                // flipped away by the drawer is not there to be clicked, and
                // its mirror image is.
                let arm = vector * side(vector.dot(view));
                if vector.dot(view).abs() <= 0.99 {
                    if let Some(t) = ray_capsule(
                        ray,
                        pivot + arm * (SCREEN_RADIUS * scale),
                        pivot + arm * (AXIS_LENGTH * scale),
                        AXIS_RADIUS * scale,
                    ) {
                        hits.push(GizmoHit {
                            handle: GizmoHandle::TranslateAxis(axis),
                            ray_t: t,
                        });
                    }
                }
                if vector.dot(view).abs() >= 0.2 {
                    let (u, v) = plane_basis(axis);
                    let center = pivot
                        + u * (PLANE_OFFSET * scale * side(u.dot(view)))
                        + v * (PLANE_OFFSET * scale * side(v.dot(view)));
                    if let Some(t) = ray_disc(ray, center, vector, PLANE_RADIUS * scale) {
                        hits.push(GizmoHit {
                            handle: GizmoHandle::TranslatePlane(axis),
                            ray_t: t,
                        });
                    }
                }
            }
            if let Some(t) = ray_disc(ray, pivot, view, SCREEN_RADIUS * scale) {
                hits.push(GizmoHit {
                    handle: GizmoHandle::TranslateScreen,
                    ray_t: t,
                });
            }
        }
        GizmoMode::Rotate => {
            for axis in [Axis::X, Axis::Y, Axis::Z] {
                if let Some(t) = ray_ring(
                    ray,
                    pivot,
                    axis.vector(),
                    RING_RADIUS * scale,
                    RING_WIDTH * scale,
                ) {
                    hits.push(GizmoHit {
                        handle: GizmoHandle::RotateAxis(axis),
                        ray_t: t,
                    });
                }
            }
            if let Some(t) = ray_ring(ray, pivot, view, RING_RADIUS * scale, RING_WIDTH * scale) {
                hits.push(GizmoHit {
                    handle: GizmoHandle::RotateScreen,
                    ray_t: t,
                });
            }
        }
    }
    hits.into_iter().min_by(|a, b| a.ray_t.total_cmp(&b.ray_t))
}

fn plane_basis(normal: Axis) -> (Vec3, Vec3) {
    match normal {
        Axis::X => (Vec3::Y, Vec3::Z),
        Axis::Y => (Vec3::X, Vec3::Z),
        Axis::Z => (Vec3::X, Vec3::Y),
    }
}

fn ray_plane(ray: Ray, point: Vec3, normal: Vec3) -> Option<(f32, Vec3)> {
    let denominator = ray.dir.dot(normal);
    if denominator.abs() <= PARALLEL_EPSILON {
        return None;
    }
    let t = (point - ray.origin).dot(normal) / denominator;
    (t >= 0.0).then(|| (t, ray.at(t)))
}

fn ray_disc(ray: Ray, center: Vec3, normal: Vec3, radius: f32) -> Option<f32> {
    let (t, point) = ray_plane(ray, center, normal)?;
    ((point - center).length_squared() <= radius * radius).then_some(t)
}

fn ray_ring(ray: Ray, center: Vec3, normal: Vec3, radius: f32, width: f32) -> Option<f32> {
    let (t, point) = ray_plane(ray, center, normal)?;
    let distance = (point - center).length();
    ((distance - radius).abs() <= width).then_some(t)
}

/// Closest points between a ray and a finite segment, used as a capsule test.
fn ray_capsule(ray: Ray, a: Vec3, b: Vec3, radius: f32) -> Option<f32> {
    let segment = b - a;
    let offset = ray.origin - a;
    let aa = ray.dir.length_squared();
    let bb = ray.dir.dot(segment);
    let cc = segment.length_squared();
    let dd = ray.dir.dot(offset);
    let ee = segment.dot(offset);
    let denominator = aa * cc - bb * bb;
    let mut segment_t = if denominator.abs() > PARALLEL_EPSILON {
        (aa * ee - bb * dd) / denominator
    } else {
        0.0
    }
    .clamp(0.0, 1.0);
    let mut ray_t = (bb * segment_t - dd) / aa;
    if ray_t < 0.0 {
        ray_t = 0.0;
        segment_t = (ee / cc).clamp(0.0, 1.0);
    }
    let on_ray = ray.at(ray_t);
    let on_segment = a + segment * segment_t;
    (on_ray.distance_squared(on_segment) <= radius * radius).then_some(ray_t)
}

/// Mean of authored anchors. An empty selection has no pivot.
#[must_use]
pub fn selection_pivot(targets: &[TransformTarget]) -> Option<Vec3> {
    (!targets.is_empty())
        .then(|| targets.iter().map(|target| target.anchor).sum::<Vec3>() / targets.len() as f32)
}

#[must_use]
pub fn apply_translation(target: TransformTarget, delta: Vec3) -> TransformTarget {
    TransformTarget {
        position: target.position + delta,
        anchor: target.anchor + delta,
        ..target
    }
}

/// Apply one world-space rotation delta. Individual mode orbits the object's
/// origin about its own authored anchor; group mode orbits it about `pivot`.
#[must_use]
pub fn apply_rotation(
    target: TransformTarget,
    delta: Quat,
    pivot: Vec3,
    mode: PivotMode,
) -> TransformTarget {
    let centre = match mode {
        PivotMode::Individual => target.anchor,
        PivotMode::Group => pivot,
    };
    TransformTarget {
        position: centre + delta * (target.position - centre),
        rotation: (delta * target.rotation).normalize(),
        anchor: centre + delta * (target.anchor - centre),
    }
}

/// Snap radians to the nearest 15° increment.
#[must_use]
pub fn snap_angle_15(radians: f32) -> f32 {
    const STEP: f32 = std::f32::consts::PI / 12.0;
    (radians / STEP).round() * STEP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_axes_are_analytic_and_edge_on_axes_hide() {
        let pivot = Vec3::ZERO;
        for (axis, ray) in [
            (Axis::X, Ray::new(Vec3::new(0.7, 0.0, 3.0), -Vec3::Z)),
            (Axis::Y, Ray::new(Vec3::new(0.0, 0.7, 3.0), -Vec3::Z)),
            (Axis::Z, Ray::new(Vec3::new(0.0, -3.0, 0.7), Vec3::Y)),
        ] {
            let hit = hit_test_gizmo(
                ray,
                pivot,
                1.0,
                Vec3::new(0.2, 0.9, 0.3),
                GizmoMode::Translate,
            )
            .expect("axis should hit");
            assert_eq!(hit.handle, GizmoHandle::TranslateAxis(axis));
        }
        let hidden = hit_test_gizmo(
            Ray::new(Vec3::new(-3.0, 0.0, 0.7), Vec3::X),
            pivot,
            1.0,
            Vec3::Z,
            GizmoMode::Translate,
        );
        assert!(hidden.is_none_or(|hit| hit.handle != GizmoHandle::TranslateAxis(Axis::Z)));
    }

    /// The drawer mirrors every arm toward the viewer (`overlay.rs`'s flip
    /// pass), so from behind, the X handle lives at −X and there is nothing at
    /// +X to click. Picking the axis by name rather than by where it is drawn
    /// is how half the arms became dead.
    #[test]
    fn an_arm_is_picked_where_it_is_drawn_not_where_its_axis_points() {
        let from_behind = Vec3::new(-0.9, 0.2, 0.3);
        let drawn = hit_test_gizmo(
            Ray::new(Vec3::new(-0.7, 0.0, 3.0), -Vec3::Z),
            Vec3::ZERO,
            1.0,
            from_behind,
            GizmoMode::Translate,
        )
        .expect("the mirrored X arm should hit");
        assert_eq!(drawn.handle, GizmoHandle::TranslateAxis(Axis::X));
        assert!(
            hit_test_gizmo(
                Ray::new(Vec3::new(0.7, 0.0, 3.0), -Vec3::Z),
                Vec3::ZERO,
                1.0,
                from_behind,
                GizmoMode::Translate,
            )
            .is_none(),
            "nothing is drawn at +X from this side"
        );
    }

    #[test]
    fn plane_screen_and_rotate_handles_hit_their_closed_forms() {
        let plane = hit_test_gizmo(
            Ray::new(Vec3::new(0.22, 0.22, 3.0), -Vec3::Z),
            Vec3::ZERO,
            1.0,
            Vec3::new(0.3, 0.4, 1.0),
            GizmoMode::Translate,
        )
        .expect("xy plane should hit");
        assert_eq!(plane.handle, GizmoHandle::TranslatePlane(Axis::Z));

        let screen = hit_test_gizmo(
            Ray::new(Vec3::new(0.0, 0.0, 3.0), -Vec3::Z),
            Vec3::ZERO,
            1.0,
            Vec3::Z,
            GizmoMode::Translate,
        )
        .expect("screen disc should hit");
        assert_eq!(screen.handle, GizmoHandle::TranslateScreen);

        let rotate = hit_test_gizmo(
            Ray::new(Vec3::new(0.8, 0.0, 3.0), -Vec3::Z),
            Vec3::ZERO,
            1.0,
            Vec3::new(0.2, 0.3, 1.0),
            GizmoMode::Rotate,
        )
        .expect("z ring should hit");
        assert!(matches!(
            rotate.handle,
            GizmoHandle::RotateAxis(Axis::Z) | GizmoHandle::RotateScreen
        ));
    }

    #[test]
    fn misses_remain_misses_over_a_scale_sweep() {
        for scale in [0.01, 0.1, 1.0, 10.0, 100.0] {
            let ray = Ray::new(Vec3::new(4.0 * scale, 4.0 * scale, 3.0), -Vec3::Z);
            assert_eq!(
                hit_test_gizmo(ray, Vec3::ZERO, scale, Vec3::Z, GizmoMode::Translate),
                None
            );
        }
    }

    fn target(position: Vec3, anchor: Vec3) -> TransformTarget {
        TransformTarget {
            position,
            rotation: Quat::IDENTITY,
            anchor,
        }
    }

    #[test]
    fn selection_pivot_is_the_anchor_centroid_and_translation_moves_both() {
        let targets = [
            target(Vec3::ZERO, Vec3::new(-2.0, 0.0, 0.0)),
            target(Vec3::ZERO, Vec3::new(2.0, 2.0, 0.0)),
        ];
        assert_eq!(selection_pivot(&targets), Some(Vec3::new(0.0, 1.0, 0.0)));
        assert_eq!(selection_pivot(&[]), None);
        let moved = apply_translation(targets[0], Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(moved.position, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(moved.anchor, Vec3::new(-1.0, 2.0, 3.0));
    }

    #[test]
    fn group_and_individual_rotation_use_their_declared_pivots() {
        let quarter = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let original = target(Vec3::new(2.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0));
        let individual = apply_rotation(original, quarter, Vec3::ZERO, PivotMode::Individual);
        assert!(individual
            .position
            .abs_diff_eq(Vec3::new(1.0, 1.0, 0.0), 1e-6));
        assert!(individual.anchor.abs_diff_eq(original.anchor, 1e-6));

        let group = apply_rotation(original, quarter, Vec3::ZERO, PivotMode::Group);
        assert!(group.position.abs_diff_eq(Vec3::new(0.0, 2.0, 0.0), 1e-6));
        assert!(group.anchor.abs_diff_eq(Vec3::new(0.0, 1.0, 0.0), 1e-6));
    }

    #[test]
    fn fifteen_degree_snap_is_bounded_and_idempotent() {
        let step = std::f32::consts::PI / 12.0;
        for i in -720..=720 {
            let angle = i as f32 * 0.01;
            let snapped = snap_angle_15(angle);
            assert!((snapped - angle).abs() <= step * 0.5 + 1e-6);
            assert!((snap_angle_15(snapped) - snapped).abs() <= 1e-6);
            assert!(((snapped / step) - (snapped / step).round()).abs() <= 2e-5);
        }
    }
}
