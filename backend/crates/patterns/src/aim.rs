//! Aim math: directions in the room (U stage right, V downstage, Z up), the
//! offset frame at an aim, leans, and how a layer combines with the aim under
//! it: a Replace clip blends toward its own aim along the shortest arc, and
//! an Offset clip turns the aim under it. A score never holds pan or tilt; a
//! solver turns aims into pan and tilt.
use crate::runtime::EvaluatedValue;
use crate::{Error, Result};
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

/// The graph output on which an aim form gives each head its [`Turn`].
pub const TURN_OUTPUT: &str = "turn";

/// The channels of a [`Turn`] on the wire: lean U, V, Z, yaw, pitch, mirror
/// U, V, Z, alpha.
pub const TURN_CHANNELS: usize = 9;

/// What an Offset aim clip does to one head: its fan's lean and its motion's
/// yaw and pitch, which turn whatever aim is under the clip. The degrees are
/// at full alpha; [`offset_aim`] scales them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Turn {
    /// The way the fan leans the head, its length the degrees. Zero is no
    /// lean. A head beyond the mirror already leans the mirror image.
    pub lean: [f64; 3],
    /// Degrees toward the right of the aim.
    pub yaw: f64,
    /// Degrees toward the up of the aim.
    pub pitch: f64,
    /// The mirror plane's normal, for a head on its low side.
    pub mirror: Option<[f64; 3]>,
    /// 0 to 1.
    pub alpha: f64,
}

impl Turn {
    /// The turn on [`TURN_CHANNELS`] wire channels. A zero mirror is none.
    #[must_use]
    pub fn from_channels(ch: [f64; TURN_CHANNELS]) -> Self {
        let mirror = [ch[5], ch[6], ch[7]];
        Self {
            lean: [ch[0], ch[1], ch[2]],
            yaw: ch[3],
            pitch: ch[4],
            mirror: (dot(mirror, mirror) > 1e-18).then_some(mirror),
            alpha: ch[8].clamp(0.0, 1.0),
        }
    }

    #[must_use]
    pub fn channels(&self) -> [f64; TURN_CHANNELS] {
        let [lu, lv, lz] = self.lean;
        let [mu, mv, mz] = self.mirror.unwrap_or([0.0; 3]);
        [lu, lv, lz, self.yaw, self.pitch, mu, mv, mz, self.alpha]
    }

    /// Each head's turn at each sample of `value`, an aim graph's
    /// [`TURN_OUTPUT`]: one row per sample of `times`, one turn per head of
    /// `fixtures`, in order.
    ///
    /// # Errors
    /// Fails when `value` is not a turn signal or lacks a head.
    pub fn read(
        value: &EvaluatedValue,
        fixtures: &[String],
        times: usize,
    ) -> Result<Vec<Vec<Self>>> {
        let signal = value
            .signal()
            .filter(|signal| signal.values().dim().2 == TURN_CHANNELS)
            .ok_or_else(|| Error("an aim turn needs nine channels".into()))?;
        let rows = fixtures
            .iter()
            .map(|id| match signal.fixtures() {
                None => Ok(0),
                Some(heads) => heads
                    .iter()
                    .position(|head| head == id)
                    .ok_or_else(|| Error(format!("the aim turn is missing head {id}"))),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((0..times)
            .map(|t| {
                rows.iter()
                    .map(|&n| Self::from_channels(std::array::from_fn(|ch| signal.at(n, t, ch))))
                    .collect()
            })
            .collect())
    }

    /// `d` leaned, then yawed and pitched, with the degrees times `scale`.
    fn apply(&self, d: [f64; 3], scale: f64) -> [f64; 3] {
        let degrees = dot(self.lean, self.lean).sqrt();
        let leaned = lean(d, self.lean, degrees * scale);
        mirrored_offset(leaned, self.yaw * scale, self.pitch * scale, self.mirror)
    }
}

/// Turn the aim under an Offset clip by the clip's `turn`, with every angle
/// times alpha. Alpha 0 leaves the aim under it. Over no aim, the turn starts
/// from the head's `home`. The weight follows [`blend_aim`]: alpha over no
/// aim, and `w + (1 − w) × alpha` over an aim of weight `w`.
#[must_use]
pub fn offset_aim(under: Option<Aim>, home: [f64; 3], turn: &Turn) -> Option<Aim> {
    let alpha = turn.alpha.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return under;
    }
    let (from, below) = match under.filter(|aim| aim.weight > 0.0) {
        Some(aim) => (aim.direction, aim.weight.clamp(0.0, 1.0)),
        None => (home, 0.0),
    };
    Some(Aim {
        direction: turn.apply(from, alpha),
        weight: below + (1.0 - below) * alpha,
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

/// [`offset`] for a head beyond a mirror plane of unit normal `mirror`: the
/// mirror image of the offset. Reflect the aim, offset it there, reflect
/// back; the aim itself is kept.
pub fn mirrored_offset(d: [f64; 3], yaw: f64, pitch: f64, mirror: Option<[f64; 3]>) -> [f64; 3] {
    match mirror {
        Some(normal) => reflect(offset(reflect(d, normal), yaw, pitch), normal),
        None => offset(d, yaw, pitch),
    }
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
