//! Triangle meshes and the primitives the builder makes them from.
//!
//! Vertex placement follows the reference builder's primitives (trimesh's
//! `box`, `cylinder`, `revolve`, `annulus`, `cone`, `uv_sphere`, `torus`, and
//! the builder's own lofts and super-ellipsoids), because a fixture's outline
//! is the outline of those vertices: a 14-sided emitter does not reach as far
//! as a circle would. Triangulation and winding are this crate's own; every
//! closed primitive is wound outward.

use std::f64::consts::{PI, TAU};

use glam::{DMat4, DVec3};

/// A mesh being built: f64 positions and triangles.
#[derive(Debug, Clone, Default)]
pub(crate) struct Mesh {
    pub v: Vec<DVec3>,
    pub f: Vec<[u32; 3]>,
}

impl Mesh {
    pub fn transform(&mut self, m: &DMat4) {
        for p in &mut self.v {
            *p = m.transform_point3(*p);
        }
        if m.determinant() < 0.0 {
            self.invert();
        }
    }

    pub fn scale(&mut self, s: DVec3) {
        for p in &mut self.v {
            *p *= s;
        }
        if s.x * s.y * s.z < 0.0 {
            self.invert();
        }
    }

    pub fn invert(&mut self) {
        for t in &mut self.f {
            t.swap(1, 2);
        }
    }

    fn signed_volume(&self) -> f64 {
        self.f
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| self.v[i as usize]);
                a.dot(b.cross(c))
            })
            .sum::<f64>()
            / 6.0
    }

    /// Wind a closed mesh so its normals point out.
    pub fn outward(mut self) -> Self {
        if self.signed_volume() < 0.0 {
            self.invert();
        }
        self
    }

    pub fn append(&mut self, other: &Mesh) {
        let base = self.v.len() as u32;
        self.v.extend_from_slice(&other.v);
        self.f.extend(
            other
                .f
                .iter()
                .map(|t| [t[0] + base, t[1] + base, t[2] + base]),
        );
    }

    /// Drop vertices no triangle uses (a hemisphere cut from a sphere).
    pub fn compact(&mut self) {
        let mut map = vec![u32::MAX; self.v.len()];
        let mut v = Vec::new();
        for t in &mut self.f {
            for i in t.iter_mut() {
                let m = &mut map[*i as usize];
                if *m == u32::MAX {
                    *m = v.len() as u32;
                    v.push(self.v[*i as usize]);
                }
                *i = *m;
            }
        }
        self.v = v;
    }
}

/// numpy's `linspace(a, b, n)` (endpoint included), value for value.
pub(crate) fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
    match n {
        0 => Vec::new(),
        1 => vec![a],
        _ => {
            let step = (b - a) / (n - 1) as f64;
            let mut out: Vec<f64> = (0..n).map(|i| i as f64 * step + a).collect();
            out[n - 1] = b;
            out
        }
    }
}

/// numpy's `linspace(a, b, n, endpoint=False)`.
pub(crate) fn linspace_open(a: f64, b: f64, n: usize) -> Vec<f64> {
    let step = (b - a) / n as f64;
    (0..n).map(|i| i as f64 * step + a).collect()
}

/// numpy's `interp(x, xp, fp)` for increasing `xp`: clamped at the ends.
pub(crate) fn interp(x: f64, xp: &[f64], fp: &[f64]) -> f64 {
    let n = xp.len();
    if n == 0 {
        return 0.0;
    }
    if x <= xp[0] {
        return fp[0];
    }
    if x >= xp[n - 1] {
        return fp[n - 1];
    }
    let j = xp.partition_point(|&p| p <= x) - 1;
    let slope = (fp[j + 1] - fp[j]) / (xp[j + 1] - xp[j]);
    slope * (x - xp[j]) + fp[j]
}

/// Revolve a `(radius, z)` profile about Z in `sections` steps, as trimesh's
/// `revolve` does for a full turn. Slices with zero area are dropped.
pub(crate) fn revolve(profile: &[[f64; 2]], sections: usize) -> Mesh {
    let per = profile.len();
    let mut m = Mesh::default();
    for k in 0..sections {
        let th = k as f64 * (TAU / sections as f64);
        let (s, c) = th.sin_cos();
        for p in profile {
            m.v.push(DVec3::new(c * p[0], s * p[0], p[1]));
        }
    }
    let n = (sections * per) as u32;
    for k in 0..sections {
        for i in 0..per.saturating_sub(1) {
            let a = (k * per + i) as u32;
            let b = (a + per as u32) % n;
            for t in [[a, b, a + 1], [a + 1, b, (b + 1) % n]] {
                let [p, q, r] = t.map(|i| m.v[i as usize]);
                if (q - p).cross(r - p).length() > 1e-12 {
                    m.f.push(t);
                }
            }
        }
    }
    m.outward()
}

/// A box of edge lengths `e`, centred.
pub(crate) fn cuboid(e: DVec3) -> Mesh {
    let h = e * 0.5;
    let v = (0..8)
        .map(|i| {
            DVec3::new(
                if i & 1 == 0 { -h.x } else { h.x },
                if i & 2 == 0 { -h.y } else { h.y },
                if i & 4 == 0 { -h.z } else { h.z },
            )
        })
        .collect();
    let f = vec![
        [0, 2, 1],
        [1, 2, 3],
        [4, 5, 6],
        [5, 7, 6],
        [0, 1, 4],
        [1, 5, 4],
        [2, 6, 3],
        [3, 6, 7],
        [0, 4, 2],
        [2, 4, 6],
        [1, 3, 5],
        [3, 7, 5],
    ];
    Mesh { v, f }.outward()
}

/// trimesh `cylinder`: radius `r`, height `h` along Z, centred.
pub(crate) fn cylinder(r: f64, h: f64, sections: usize) -> Mesh {
    let z = h.abs() / 2.0;
    revolve(&[[0.0, -z], [r, -z], [r, z], [0.0, z]], sections)
}

/// trimesh `cone`: base radius `r` at z = 0, apex at z = `h`.
pub(crate) fn cone(r: f64, h: f64, sections: usize) -> Mesh {
    revolve(&[[0.0, 0.0], [r, 0.0], [0.0, h]], sections)
}

/// trimesh `annulus`: a tube, solid when the inner radius vanishes.
pub(crate) fn annulus(r_min: f64, r_max: f64, h: f64, sections: usize) -> Mesh {
    let r_min = r_min.abs();
    if r_min < 1e-8 {
        return cylinder(r_max, h, sections);
    }
    let r_max = r_max.abs();
    let z = h.abs() / 2.0;
    revolve(
        &[
            [r_min, -z],
            [r_max, -z],
            [r_max, z],
            [r_min, z],
            [r_min, -z],
        ],
        sections,
    )
}

/// trimesh `uv_sphere(radius, count)`.
pub(crate) fn uv_sphere(radius: f64, count: [usize; 2]) -> Mesh {
    let lat = count[0] + count[0] % 2;
    let lon = (count[1] + count[1] % 2) * 2;
    let profile: Vec<[f64; 2]> = linspace(0.0, PI, lat)
        .into_iter()
        .map(|t| [t.sin() * radius, -t.cos() * radius])
        .collect();
    revolve(&profile, lon)
}

/// trimesh `torus` about Z.
pub(crate) fn torus(major: f64, minor: f64, major_sections: usize, minor_sections: usize) -> Mesh {
    let profile: Vec<[f64; 2]> = linspace(0.0, TAU, minor_sections + 1)
        .into_iter()
        .map(|p| [minor * p.cos() + major, minor * p.sin()])
        .collect();
    revolve(&profile, major_sections)
}

/// A superellipse outline (exponent `p`: 2 round .. 16 sharp box), corners
/// clipped by `ch` (0..1, an octagon-like chamfer).
pub(crate) fn super_ring(rx: f64, ry: f64, p: f64, nseg: usize, ch: f64) -> Vec<(f64, f64)> {
    let e = 2.0 / p;
    linspace_open(0.0, TAU, nseg)
        .into_iter()
        .map(|th| {
            let (s, c) = th.sin_cos();
            let mut x = rx * sign(c) * c.abs().powf(e);
            let mut y = ry * sign(s) * s.abs().powf(e);
            if ch > 0.0 {
                let k = 1f64.min(
                    (2.0 - 1.2 * ch.min(0.9))
                        / (x.abs() / rx.max(1e-6) + y.abs() / ry.max(1e-6)).max(1e-6),
                );
                x *= k;
                y *= k;
            }
            (x, y)
        })
        .collect()
}

/// numpy's `sign`: zero stays zero.
pub(crate) fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// One loft section: a superellipse at `z`, optionally chamfered and offset.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sec {
    pub z: f64,
    pub rx: f64,
    pub ry: f64,
    pub p: f64,
    pub ch: f64,
    pub cx: f64,
    pub cy: f64,
}

impl Sec {
    pub fn new(z: f64, rx: f64, ry: f64, p: f64) -> Self {
        Self {
            z,
            rx,
            ry,
            p,
            ch: 0.0,
            cx: 0.0,
            cy: 0.0,
        }
    }

    pub fn ch(mut self, ch: f64) -> Self {
        self.ch = ch;
        self
    }

    pub fn at(mut self, cx: f64, cy: f64) -> Self {
        self.cx = cx;
        self.cy = cy;
        self
    }
}

/// A closed mesh through `secs` along Z, capped at both ends.
pub(crate) fn loft(secs: &[Sec], nseg: usize) -> Mesh {
    let n = secs.len();
    let mut m = Mesh::default();
    for s in secs {
        for (x, y) in super_ring(s.rx.max(1e-5), s.ry.max(1e-5), s.p, nseg, s.ch) {
            m.v.push(DVec3::new(x + s.cx, y + s.cy, s.z));
        }
    }
    let (first, last) = (secs[0], secs[n - 1]);
    m.v.push(DVec3::new(first.cx, first.cy, first.z));
    m.v.push(DVec3::new(last.cx, last.cy, last.z));
    let ns = nseg as u32;
    for k in 0..(n as u32 - 1) {
        for i in 0..ns {
            let i2 = (i + 1) % ns;
            let (a, b, c, d) = (k * ns + i, k * ns + i2, (k + 1) * ns + i, (k + 1) * ns + i2);
            m.f.push([a, b, d]);
            m.f.push([a, d, c]);
        }
    }
    let (c0, c1) = (n as u32 * ns, n as u32 * ns + 1);
    let top = (n as u32 - 1) * ns;
    for i in 0..ns {
        let i2 = (i + 1) % ns;
        m.f.push([c0, i2, i]);
        m.f.push([c1, top + i, top + i2]);
    }
    m.outward()
}

/// A hollow superellipse ring from `z0` to `z1`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ring(
    rxo: f64,
    ryo: f64,
    rxi: f64,
    ryi: f64,
    z0: f64,
    z1: f64,
    p: f64,
    ch: f64,
    n: usize,
) -> Mesh {
    let outer = super_ring(rxo, ryo, p, n, ch);
    let inner = super_ring(rxi.max(1e-4), ryi.max(1e-4), p, n, ch);
    let mut m = Mesh::default();
    for (ring, z) in [(&outer, z0), (&outer, z1), (&inner, z1), (&inner, z0)] {
        m.v.extend(ring.iter().map(|&(x, y)| DVec3::new(x, y, z)));
    }
    let n = n as u32;
    for k in 0..4 {
        for i in 0..n {
            let i2 = (i + 1) % n;
            let (a, b) = (k * n + i, k * n + i2);
            let (c, d) = (((k + 1) % 4) * n + i, ((k + 1) % 4) * n + i2);
            m.f.push([a, b, d]);
            m.f.push([a, d, c]);
        }
    }
    m.outward()
}

/// A super-ellipsoid "rounded box"; `e` is the rounding exponent (small = boxier).
pub(crate) fn rbox(size: DVec3, e: f64, nu: usize, nv: usize) -> Mesh {
    let [a, b, c] = size.to_array().map(|v| v.max(1e-4) / 2.0);
    let th = linspace_open(0.0, TAU, nu);
    let ph = linspace(-PI / 2.0, PI / 2.0, nv + 1);
    let sp = |x: f64| sign(x) * x.abs().powf(e);
    let mut m = Mesh::default();
    for &p in &ph {
        for &t in &th {
            m.v.push(DVec3::new(
                a * sp(p.cos()) * sp(t.cos()),
                b * sp(p.cos()) * sp(t.sin()),
                c * sp(p.sin()),
            ));
        }
    }
    let nu32 = nu as u32;
    for j in 0..nv as u32 {
        for i in 0..nu32 {
            let i2 = (i + 1) % nu32;
            let (p, q, r, s) = (
                j * nu32 + i,
                j * nu32 + i2,
                (j + 1) * nu32 + i,
                (j + 1) * nu32 + i2,
            );
            m.f.push([p, q, s]);
            m.f.push([p, s, r]);
        }
    }
    m.outward()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_are_closed_and_outward() {
        for m in [
            cuboid(DVec3::new(1.0, 2.0, 3.0)),
            cylinder(0.5, 1.0, 28),
            annulus(0.4, 0.5, 0.1, 40),
            uv_sphere(1.0, [5, 8]),
            torus(1.0, 0.2, 32, 10),
            loft(
                &[Sec::new(0.0, 1.0, 1.0, 2.0), Sec::new(1.0, 0.5, 0.5, 6.0)],
                36,
            ),
            ring(1.0, 1.0, 0.8, 0.8, 0.0, 0.1, 6.0, 0.0, 44),
            rbox(DVec3::ONE, 0.3, 24, 14),
        ] {
            assert!(m.signed_volume() > 0.0);
        }
    }

    #[test]
    fn linspace_and_interp_match_numpy() {
        assert_eq!(linspace(0.0, 1.0, 5), vec![0.0, 0.25, 0.5, 0.75, 1.0]);
        assert_eq!(interp(0.5, &[0.0, 1.0], &[2.0, 4.0]), 3.0);
        assert_eq!(interp(-1.0, &[0.0, 1.0], &[2.0, 4.0]), 2.0);
        assert_eq!(interp(2.0, &[0.0, 1.0], &[2.0, 4.0]), 4.0);
    }
}
