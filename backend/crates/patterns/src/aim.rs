//! Aim math: directions in the room (U stage right, V downstage, Z up), the
//! offset frame at an aim, leans, and the shortest-arc blend between layers.
//! A score never holds pan or tilt; a solver turns aims into pan and tilt.
use serde::{Deserialize, Serialize};

/// Straight up.
const UP: [f64; 3] = [0.0, 0.0, 1.0];
/// Straight down: the aim a head takes when its vector has no direction.
pub const DOWN: [f64; 3] = [0.0, 0.0, -1.0];

/// One head's aim after the layers under it. `weight` is how much of the aim
/// the clips set; the rest is the head's home. The solver aims the head at
/// `slerp(home, direction, weight)`. Weight 1 is fully set by clips; weight
/// 0 is no aim.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Aim {
    /// A unit vector in U, V, Z.
    pub direction: [f64; 3],
    /// 0 to 1.
    pub weight: f64,
}

/// Blend a clip's aim (`top`, with its alpha as weight) onto the aim under
/// it, along the shortest arc. Alpha 0 leaves the aim under it. Over no aim,
/// the clip blends from home: the result keeps the clip's direction with its
/// alpha as weight. Over an aim of weight `w`, the new weight is
/// `w + (1 − w) × alpha`. When the aim under it is fully set (`w = 1`, the
/// usual case), the result is exactly `slerp(under, top, alpha)`.
pub fn blend_aim(under: Option<Aim>, top: Aim) -> Option<Aim> {
    let alpha = top.weight.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return under;
    }
    let Some(under) = under.filter(|aim| aim.weight > 0.0) else {
        return Some(Aim {
            direction: top.direction,
            weight: alpha,
        });
    };
    let below = under.weight.clamp(0.0, 1.0);
    let weight = below + (1.0 - below) * alpha;
    Some(Aim {
        direction: slerp(under.direction, top.direction, alpha / weight),
        weight,
    })
}

/// Unit-vector interpolation along the shortest arc. Opposite vectors turn
/// through the aim's up direction.
pub fn slerp(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    let (a, b) = (unit(a), unit(b));
    let cos = dot(a, b).clamp(-1.0, 1.0);
    if cos > 1.0 - 1e-12 {
        return unit(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t));
    }
    let (toward, angle) = if cos < -1.0 + 1e-12 {
        (frame(a).1, std::f64::consts::PI)
    } else {
        let across: [f64; 3] = std::array::from_fn(|i| b[i] - cos * a[i]);
        (unit(across), cos.acos())
    };
    let (sin, cos) = (angle * t).sin_cos();
    unit(std::array::from_fn(|i| a[i] * cos + toward[i] * sin))
}

/// The offset frame at aim `d`: `right = normalize(d × Z)` and
/// `up = right × d`. Straight up or down, right is stage right.
pub fn frame(d: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let d = unit(d);
    let across = cross(d, UP);
    let right = if dot(across, across) < 1e-18 {
        [1.0, 0.0, 0.0]
    } else {
        unit(across)
    };
    (right, cross(right, d))
}

/// Turn `d` by `yaw` degrees toward the frame's right and `pitch` degrees
/// toward its up.
pub fn offset(d: [f64; 3], yaw: f64, pitch: f64) -> [f64; 3] {
    let d = unit(d);
    let (right, up) = frame(d);
    let (sy, cy) = yaw.to_radians().sin_cos();
    let (sp, cp) = pitch.to_radians().sin_cos();
    unit(std::array::from_fn(|i| {
        cp * (cy * d[i] + sy * right[i]) + sp * up[i]
    }))
}

/// Rotate `d` by `degrees` toward `toward`, around `d × toward`. A negative
/// angle leans away. When `toward` is parallel to `d`, `d` does not move.
pub fn lean(d: [f64; 3], toward: [f64; 3], degrees: f64) -> [f64; 3] {
    let d = unit(d);
    let along = dot(toward, d);
    let across: [f64; 3] = std::array::from_fn(|i| toward[i] - along * d[i]);
    if dot(across, across) < 1e-18 || degrees == 0.0 {
        return d;
    }
    let across = unit(across);
    let (sin, cos) = degrees.to_radians().sin_cos();
    unit(std::array::from_fn(|i| cos * d[i] + sin * across[i]))
}

/// `v` reflected across the plane through the origin with unit normal `n`:
/// the component along `n` flips.
pub fn reflect(v: [f64; 3], n: [f64; 3]) -> [f64; 3] {
    let along = dot(v, n);
    std::array::from_fn(|i| v[i] - 2.0 * along * n[i])
}

/// The unit vector of `v`, or straight down when `v` has no direction.
pub fn unit(v: [f64; 3]) -> [f64; 3] {
    let length = dot(v, v).sqrt();
    if length < 1e-12 || !length.is_finite() {
        DOWN
    } else {
        v.map(|x| x / length)
    }
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-9)
    }

    #[test]
    fn slerp_follows_the_shortest_arc() {
        let half = slerp([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.5);
        let s = 0.5_f64.sqrt();
        assert!(close(half, [s, s, 0.0]), "{half:?}");
        assert!(close(slerp(DOWN, [0.0, 1.0, 0.0], 0.0), DOWN));
        assert!(close(slerp(DOWN, [0.0, 1.0, 0.0], 1.0), [0.0, 1.0, 0.0]));
    }

    #[test]
    fn blending_over_a_full_aim_is_slerp() {
        let under = Aim {
            direction: [1.0, 0.0, 0.0],
            weight: 1.0,
        };
        let top = Aim {
            direction: [0.0, 1.0, 0.0],
            weight: 0.25,
        };
        let blended = blend_aim(Some(under), top).unwrap();
        assert_eq!(blended.weight, 1.0);
        assert!(close(
            blended.direction,
            slerp(under.direction, top.direction, 0.25)
        ));
        assert_eq!(blend_aim(None, top).unwrap().weight, 0.25);
        let absent = Aim { weight: 0.0, ..top };
        assert_eq!(blend_aim(Some(under), absent), Some(under));
        assert_eq!(blend_aim(None, absent), None);
    }
}
