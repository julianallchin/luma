//! Heads: the body a face sits on, in the head frame (beam +Z, centred, length
//! `hl` along z), and `build_head`, which adds the face and any spin/slide.

use std::borrow::Cow;
use std::f64::consts::{FRAC_PI_2, PI};

use glam::{DMat4, DVec3};

use crate::face::{self, Geom};
use crate::mesh::{self, interp, linspace, Sec};
use crate::rig::{cl, fail, int, rot, ry, tr, Axis, JointRec, Res, Rig};
use crate::spec::{
    Body, BodyOutline, BodyShape, CanProfile, EmitterKind, Face, Joint, MaterialSlot as M, Shell,
    ShellSection, ZoneLayout,
};
use crate::JointKind;

/// numpy's `round(x, 3)`.
fn np_round3(x: f64) -> f64 {
    (x * 1000.0).round_ties_even() / 1000.0
}

fn prior_par() -> Vec<f64> {
    linspace(0.0, 1.0, 24)
        .into_iter()
        .map(|t| {
            np_round3(interp(
                t,
                &[0.0, 0.25, 0.5, 0.75, 1.0],
                &[0.55, 0.7, 0.85, 0.95, 1.0],
            ))
        })
        .collect()
}

fn default_profile() -> Vec<f64> {
    const PRIOR_SPOT: [f64; 9] = [0.62, 0.84, 0.97, 1.0, 0.94, 0.84, 0.74, 0.68, 0.66];
    let xp = linspace(0.0, 1.0, 9);
    linspace(0.0, 1.0, 24)
        .into_iter()
        .map(|t| np_round3(interp(t, &xp, &PRIOR_SPOT)))
        .collect()
}

fn rec_of(hl: f64) -> f64 {
    cl(0.045 * hl, 0.006, 0.03)
}

/// Corner roundness 1 (ellipse) .. 0 (sharp box) as a superellipse exponent.
pub(crate) fn rp(r: f64) -> f64 {
    if r <= 0.02 {
        60.0
    } else {
        2.0 + 14.0 * (1.0 - cl(r, 0.0, 1.0)).powi(2)
    }
}

/// Half-size fraction of the largest same-aspect rectangle inside a
/// superellipse, so square faces stay inside rounded bodies.
fn inscribed(p: f64, ch: f64) -> f64 {
    let s = 2f64.powf(-1.0 / p);
    if ch > 0.0 {
        s.min((2.0 - 1.2 * ch.min(0.9)) / 2.0)
    } else {
        s
    }
}

fn barn(b: &mut Rig, w: f64, h: f64, z: f64) {
    for (x, y, sx, sy) in [
        (0.0, h / 2.0 + 0.01, w, 0.004),
        (0.0, -h / 2.0 - 0.01, w, 0.004),
        (w / 2.0 + 0.01, 0.0, 0.004, h),
        (-w / 2.0 - 0.01, 0.0, 0.004, h),
    ] {
        b.box_("barn", [sx, sy, 0.08], [x, y, z + 0.03], M::Body2);
    }
}

/// A hollow lip around a recessed face, from `z0` to `z1`.
#[allow(clippy::too_many_arguments)]
fn bezel_ring(
    b: &mut Rig,
    name: &'static str,
    z0: f64,
    z1: f64,
    rx: f64,
    ry: f64,
    wall: f64,
    mat: M,
    rect: Option<f64>,
) {
    if let Some(p) = rect {
        // Outer edge 1% proud of the body it sits on, base just behind it.
        let m = mesh::ring(
            rx * 1.01,
            ry * 1.01,
            rx - wall,
            ry - wall,
            z0 - 0.0005,
            z1,
            p,
            0.0,
            44,
        );
        b.add(name, m, mat, DMat4::IDENTITY);
    } else {
        let mut m = mesh::annulus(1.0 - wall / rx.max(1e-4), 1.0, (z1 - z0).max(1e-4), 40);
        m.scale(DVec3::new(rx, ry, 1.0));
        b.add(name, m, mat, tr(0.0, 0.0, (z0 + z1) / 2.0));
    }
}

fn has_feature(s: &Body, f: &str) -> bool {
    s.features.iter().flatten().any(|x| x == f)
}

fn can_new(b: &mut Rig, s: &Body, prof: CanProfile, rx: f64, ry: f64, hl: f64, rec: f64) -> Geom {
    let zf = hl / 2.0;
    let r = rx.min(ry);
    let nrib = int(s.ribs.filter(|&v| v != 0.0).unwrap_or(0.0));
    let fr = match prof {
        CanProfile::Boxcan => {
            let p = s.corner.map_or(6.0, rp);
            let secs = [
                Sec::new(-hl / 2.0, rx * 0.9, ry * 0.9, p),
                Sec::new(-hl / 2.0 + hl * 0.05, rx, ry, p),
                Sec::new(zf - rec, rx, ry, p),
            ];
            b.loft("can_body", &secs, M::Body, 36, DMat4::IDENTITY);
            for i in 0..nrib {
                let z = -hl * 0.35 + i as f64 * hl * 0.7 / (nrib - 1).max(1) as f64;
                b.box_(
                    "rib",
                    [2.0 * rx * 1.04, 2.0 * ry * 1.04, hl * 0.02],
                    [0.0, 0.0, z],
                    M::Body2,
                );
            }
            let wall = 0.006f64.max(r * 0.1);
            bezel_ring(
                b,
                "bezel",
                zf - rec,
                zf,
                rx * 1.01,
                ry * 1.01,
                wall,
                M::Body2,
                Some(p),
            );
            let rnd = s.corner.unwrap_or(0.0) >= 0.85;
            let k = if rnd { 0.94 } else { inscribed(p, 0.0) * 0.97 };
            return Geom {
                fw: 2.0 * (rx - wall) * k,
                fh: 2.0 * (ry - wall) * k,
                z: zf - rec,
                rnd,
            };
        }
        CanProfile::Step2 => {
            let st = s.step.as_ref();
            let at = cl(st.and_then(|s| s.at).unwrap_or(0.55), 0.15, 0.85);
            let f = st.and_then(|s| s.r).unwrap_or(0.78);
            let zs = -hl / 2.0 + at * hl;
            let rear = [
                Sec::new(-hl / 2.0, rx * 0.9, ry * 0.9, 2.0),
                Sec::new(-hl / 2.0 + hl * 0.04, rx, ry, 2.0),
                Sec::new(zs, rx, ry, 2.0),
            ];
            b.loft("can_rear", &rear, M::Body, 40, DMat4::IDENTITY);
            let front = [
                Sec::new(zs, rx * f, ry * f, 2.0),
                Sec::new(zf - rec, rx * f, ry * f, 2.0),
            ];
            b.loft("can_front", &front, M::Body, 40, DMat4::IDENTITY);
            b.cyl(
                "step_ring",
                r * (1.0 + f) / 2.0 * 1.04,
                hl * 0.02,
                [0.0, 0.0, zs],
                Axis::Z,
                M::Body2,
                36,
            );
            bezel_ring(
                b,
                "bezel",
                zf - rec,
                zf + hl * 0.01,
                rx * f * 1.03,
                ry * f * 1.03,
                0.006f64.max(r * f * 0.1),
                M::Body2,
                None,
            );
            r * f
        }
        _ => {
            let secs = [
                Sec::new(-hl / 2.0, rx * 0.92, ry * 0.92, 2.0),
                Sec::new(-hl / 2.0 + hl * 0.04, rx, ry, 2.0),
                Sec::new(zf - rec, rx, ry, 2.0),
            ];
            b.loft("can_body", &secs, M::Body, 40, DMat4::IDENTITY);
            for i in 0..nrib {
                let z = -hl * 0.35 + i as f64 * hl * 0.7 / (nrib - 1).max(1) as f64;
                b.cyl(
                    "rib",
                    r * 1.025,
                    hl * 0.02,
                    [0.0, 0.0, z],
                    Axis::Z,
                    M::Body2,
                    36,
                );
            }
            bezel_ring(
                b,
                "bezel",
                zf - rec,
                zf + hl * 0.01,
                rx * 1.03,
                ry * 1.03,
                0.006f64.max(r * 0.1),
                M::Body2,
                None,
            );
            r
        }
    };
    b.cyl(
        "can_rear",
        fr * 0.5,
        hl * 0.04,
        [0.0, 0.0, -hl / 2.0 - hl * 0.015],
        Axis::Z,
        M::Body2,
        24,
    );
    if has_feature(s, "barn_doors") {
        barn(b, 2.0 * fr, 2.0 * fr, hl / 2.0);
    }
    Geom {
        fw: 2.0 * fr * 0.84,
        fh: 2.0 * fr * 0.84,
        z: zf - rec,
        rnd: true,
    }
}

/// Sections of a can profile `pr` up to `cut`, then the rim at `zf - rec`.
fn profile_secs(pr: &[f64], cut: f64, hl: f64, rx: f64, ry: f64) -> Vec<Sec> {
    let ts = linspace(0.0, 1.0, pr.len());
    ts.iter()
        .zip(pr)
        .filter(|(t, _)| **t <= cut)
        .map(|(t, v)| Sec::new(-hl / 2.0 + t * hl, rx * v, ry * v, 2.0))
        .collect()
}

fn body_can(b: &mut Rig, s: &Body, hw: f64, hc: f64, hl: f64) -> Res<Geom> {
    let prof = s.profile.unwrap_or(CanProfile::Bell);
    let (rx, ry) = (hw / 2.0, hc / 2.0);
    let r = rx.min(ry);
    let rec = rec_of(hl);
    let zf = hl / 2.0;
    let prof_r = s.prof_r.clone().filter(|p| !p.is_empty());
    Ok(match prof {
        CanProfile::Cylinder | CanProfile::Step2 | CanProfile::Boxcan => {
            can_new(b, s, prof, rx, ry, hl, rec)
        }
        CanProfile::Bell => {
            let pr = prof_r.unwrap_or_else(default_profile);
            let ts = linspace(0.0, 1.0, pr.len());
            let mut secs = profile_secs(&pr, 0.965, hl, rx, ry);
            let rf = match ts.iter().position(|&t| t > 0.965) {
                Some(i) => pr[(i + pr.len() - 1) % pr.len()],
                None => pr[pr.len() - 1],
            };
            secs.push(Sec::new(zf - rec, rx * rf, ry * rf, 2.0));
            b.loft("can_body", &secs, M::Body, 40, DMat4::IDENTITY);
            b.cyl(
                "can_rear",
                r * pr[0] * 0.55,
                hl * 0.04,
                [0.0, 0.0, -hl / 2.0 - hl * 0.01],
                Axis::Z,
                M::Body2,
                24,
            );
            if s.pods.unwrap_or(true) {
                let Some(widest) = pr
                    .get(10..pr.len().min(14))
                    .and_then(|w| w.iter().copied().reduce(f64::max))
                else {
                    return fail("bell profile shorter than 11 samples");
                };
                for sg in [-1.0, 1.0] {
                    b.cyl(
                        "pod",
                        r * 0.3,
                        0.008,
                        [sg * (rx * widest - 0.001), 0.0, 0.0],
                        Axis::X,
                        M::Body2,
                        20,
                    );
                }
            }
            let (rim_rx, rim_ry) = (rx * rf, ry * rf);
            bezel_ring(
                b,
                "bezel",
                zf - rec,
                zf,
                rim_rx,
                rim_ry,
                0.006f64.max(rim_rx.min(rim_ry) * 0.12),
                M::Body2,
                None,
            );
            b.cyl(
                "collar",
                r * rf * 1.04,
                hl * 0.03,
                [0.0, 0.0, zf - rec - hl * 0.05],
                Axis::Z,
                M::Body2,
                40,
            );
            let fr = rim_rx.min(rim_ry);
            Geom {
                fw: 2.0 * fr * 0.86,
                fh: 2.0 * fr * 0.86,
                z: zf - rec,
                rnd: true,
            }
        }
        CanProfile::Taper => {
            let mut pr = prof_r.unwrap_or_else(prior_par);
            if let Some([a, c]) = s.taper {
                pr = linspace(a, c, 24);
            }
            let mut secs = profile_secs(&pr, 0.95, hl, rx, ry);
            secs.push(Sec::new(zf - rec, rx, ry, 2.0));
            b.loft("can_body", &secs, M::Body, 40, DMat4::IDENTITY);
            let nrib = s.ribs.map_or(3, int);
            for i in 0..nrib {
                let z = -hl * 0.02 + i as f64 * hl * 0.09 * (3.0 / nrib.max(3) as f64);
                b.cyl(
                    "rib",
                    r * 1.025,
                    hl * 0.025,
                    [0.0, 0.0, z],
                    Axis::Z,
                    M::Body2,
                    36,
                );
            }
            b.cyl(
                "can_rear",
                r * pr[0] * 0.8,
                hl * 0.04,
                [0.0, 0.0, -hl / 2.0 - hl * 0.015],
                Axis::Z,
                M::Body2,
                24,
            );
            bezel_ring(
                b,
                "bezel",
                zf - rec,
                zf + hl * 0.01,
                rx * 1.03,
                ry * 1.03,
                0.008f64.max(r * 0.12),
                M::Body2,
                None,
            );
            if has_feature(s, "barn_doors") {
                barn(b, 2.0 * r, 2.0 * r, hl / 2.0);
            }
            Geom {
                fw: 2.0 * r * 0.84,
                fh: 2.0 * r * 0.84,
                z: zf - rec,
                rnd: true,
            }
        }
        CanProfile::Straight => {
            let secs = [
                Sec::new(-hl / 2.0, r * 0.45, r * 0.45, 2.0),
                Sec::new(-hl / 2.0 + hl * 0.08, r * 0.8, r * 0.8, 2.0),
                Sec::new(-hl / 2.0 + hl * 0.16, r, r, 2.0),
                Sec::new(zf - rec, r, r, 2.0),
            ];
            b.loft("can_body", &secs, M::Body, 40, DMat4::IDENTITY);
            b.box_(
                "handle",
                [0.02, 0.02, hl * 0.25],
                [0.0, r - 0.002, -hl * 0.15],
                M::Body2,
            );
            bezel_ring(
                b,
                "bezel",
                zf - rec,
                zf + 0.004,
                r * 1.02,
                r * 1.02,
                0.008f64.max(r * 0.1),
                M::Body2,
                None,
            );
            Geom {
                fw: 2.0 * r * 0.84,
                fh: 2.0 * r * 0.84,
                z: zf - rec,
                rnd: true,
            }
        }
        CanProfile::Stepped => {
            let r = r * 0.95;
            b.cyl(
                "barrel_rear",
                r,
                hl * 0.55,
                [0.0, 0.0, -hl * 0.22],
                Axis::Z,
                M::Body,
                28,
            );
            b.cyl(
                "rear_cap",
                r * 0.9,
                0.02,
                [0.0, 0.0, -hl * 0.5],
                Axis::Z,
                M::Body2,
                28,
            );
            b.cyl(
                "barrel_mid",
                r * 0.7,
                hl * 0.2,
                [0.0, 0.0, hl * 0.1],
                Axis::Z,
                M::Body2,
                28,
            );
            b.cyl(
                "lens_tube",
                r * 0.75,
                hl * 0.3,
                [0.0, 0.0, hl * 0.35],
                Axis::Z,
                M::Body,
                28,
            );
            bezel_ring(
                b,
                "bezel",
                hl * 0.5 - rec,
                hl * 0.5 + 0.004,
                r * 0.78,
                r * 0.78,
                0.01,
                M::Body2,
                None,
            );
            b.box_(
                "handle",
                [0.02, 0.02, hl * 0.25],
                [0.0, r + 0.015, -hl * 0.2],
                M::Body2,
            );
            Geom {
                fw: 2.0 * r * 0.66,
                fh: 2.0 * r * 0.66,
                z: hl * 0.5 - rec,
                rnd: true,
            }
        }
    })
}

fn strip_or_tube_zones(faces: &[Face]) -> bool {
    faces[0].zones.iter().flatten().any(|z| {
        matches!(
            z.emitter.as_ref().and_then(|e| e.kind),
            Some(EmitterKind::Strip | EmitterKind::Tube)
        )
    })
}

fn body_box(b: &mut Rig, s: &Body, hw: f64, hc: f64, hl: f64, faces: &[Face]) -> Geom {
    let (rx, ry) = (hw / 2.0, hc / 2.0);
    let rec = rec_of(hl);
    let zf = hl / 2.0;
    let secs: Vec<Sec> = if strip_or_tube_zones(faces) {
        let p = s.corner.map_or(7.0, rp);
        vec![
            Sec::new(-hl / 2.0, rx * 0.93, ry * 0.93, p),
            Sec::new(-hl / 2.0 + hl * 0.06, rx, ry, p),
            Sec::new(zf - hl * 0.18, rx, ry, p),
            Sec::new(zf - rec, rx * 0.98, ry * 0.98, p),
        ]
    } else {
        let p = s.corner.map_or(4.5, rp);
        vec![
            Sec::new(-hl / 2.0, rx * 0.62, ry * 0.62, p),
            Sec::new(-hl / 2.0 + hl * 0.1, rx * 0.92, ry * 0.92, p),
            Sec::new(-hl / 2.0 + hl * 0.22, rx, ry, p),
            Sec::new(zf - hl * 0.18, rx, ry, p),
            Sec::new(zf - rec, rx * 0.97, ry * 0.97, p),
        ]
    };
    b.loft("box_body", &secs, M::Body, 36, DMat4::IDENTITY);
    let p_last = secs[secs.len() - 1].p;
    let wall = 0.006f64.max(rx.min(ry) * 0.09);
    bezel_ring(
        b,
        "bezel",
        zf - rec,
        zf,
        rx * 0.97,
        ry * 0.97,
        wall,
        M::Body2,
        Some(p_last),
    );
    if s.pods.unwrap_or(true) {
        for sg in [-1.0, 1.0] {
            b.cyl(
                "pod",
                rx.min(ry) * 0.28,
                0.008,
                [sg * (rx + 0.001), 0.0, 0.0],
                Axis::X,
                M::Body2,
                20,
            );
        }
    }
    if has_feature(s, "vents") {
        for i in 0..4 {
            b.box_(
                "vent",
                [hw * 0.5, 0.004, hl * 0.1],
                [0.0, ry, -hl * 0.3 + i as f64 * hl * 0.1],
                M::Body2,
            );
        }
    }
    let rnd = s.corner.unwrap_or(0.0) >= 0.85;
    let k = if rnd {
        0.8
    } else {
        0.72f64.min(inscribed(p_last, 0.0) * 0.97 * (1.0 - 2.0 * wall / hw))
    };
    Geom {
        fw: hw * k,
        fh: hc * k,
        z: zf - rec,
        rnd,
    }
}

fn body_panel(b: &mut Rig, s: &Body, hw: f64, hc: f64, hl: f64) -> Geom {
    let rec = rec_of(hl);
    let zf = hl / 2.0;
    let (rx, ry) = (hw / 2.0, hc / 2.0);
    let p = s.corner.map_or(6.0, rp);
    let secs = [
        Sec::new(-hl / 2.0, rx * 0.72, ry * 0.72, p),
        Sec::new(-hl / 2.0 + hl * 0.25, rx * 0.98, ry * 0.98, p),
        Sec::new(zf - rec, rx, ry, p),
    ];
    b.loft("panel_body", &secs, M::Body, 32, DMat4::IDENTITY);
    b.box_(
        "panel_rear",
        [hw * 0.5, hc * 0.5, hl * 0.18],
        [0.0, 0.0, -hl / 2.0 - hl * 0.08],
        M::Body2,
    );
    let wall = 0.005f64.max(rx.min(ry) * 0.06);
    bezel_ring(
        b,
        "bezel",
        zf - rec,
        zf,
        rx * 1.01,
        ry * 1.01,
        wall,
        M::Body2,
        Some(p),
    );
    let rnd = s.corner.unwrap_or(0.0) >= 0.85;
    let k = if rnd { 0.94 } else { inscribed(p, 0.0) * 0.97 };
    Geom {
        fw: 2.0 * (rx - wall) * k,
        fh: 2.0 * (ry - wall) * k,
        z: zf - rec,
        rnd,
    }
}

fn body_disc(b: &mut Rig, hw: f64, hc: f64, hl: f64) -> Geom {
    let r = hw.min(hc) / 2.0;
    b.cyl("disc_body", r, hl, [0.0, 0.0, 0.0], Axis::Z, M::Body2, 36);
    bezel_ring(
        b,
        "disc_bezel",
        hl / 2.0 - hl * 0.15,
        hl / 2.0,
        r * 1.04,
        r * 1.04,
        r * 0.08,
        M::Metal,
        None,
    );
    Geom {
        fw: 2.0 * r * 0.88,
        fh: 2.0 * r * 0.88,
        z: hl / 2.0 - hl * 0.15,
        rnd: true,
    }
}

fn body_dome(b: &mut Rig, s: &Body, hw: f64, hl: f64) {
    let r = hw / 2.0;
    let mut sph = mesh::uv_sphere(r, [16, 20]);
    if !s.sphere.unwrap_or(false) {
        let v = sph.v.clone();
        sph.f
            .retain(|t| t.iter().all(|&i| v[i as usize].z >= -1e-6));
        sph.compact();
    }
    b.at(tr(0.0, 0.0, -hl / 2.0), |b| {
        b.add("dome_shell", sph, M::Diffuser, DMat4::IDENTITY);
        b.cyl(
            "dome_base",
            r * 1.05,
            0.02,
            [0.0, 0.0, -0.01],
            Axis::Z,
            M::Body2,
            32,
        );
    });
}

/// The face's lens plane `[w, h]` when the spec measured one.
fn face_plane(faces: &[Face]) -> Option<[f64; 2]> {
    let pl = |p: &Option<Vec<f64>>| p.as_ref().filter(|p| p.len() >= 2).map(|p| [p[0], p[1]]);
    faces[0]
        .root
        .as_ref()
        .and_then(|r| pl(&r.plane))
        .or_else(|| faces[0].zones.iter().flatten().find_map(|z| pl(&z.plane)))
}

/// Bar: length `w` (x), cross-section `h` (y), beam depth `d` (z).
fn body_bar(b: &mut Rig, s: &Body, w: f64, h: f64, d: f64, faces: &[Face]) -> Geom {
    let tube = s.outline == Some(BodyOutline::Round) && (h - d).abs() < 0.5 * h.max(d) && h < 0.12;
    if tube {
        let r = h.min(d) / 2.0;
        b.cyl("tube", r, w, [0.0; 3], Axis::X, M::Body, 28);
        b.cyl(
            "tube_diffuser",
            r * 0.9,
            w * 0.98,
            [0.0, 0.0, 0.002],
            Axis::X,
            M::Diffuser,
            28,
        );
        for sg in [-1.0, 1.0] {
            b.cyl(
                "end_cap",
                r * 1.04,
                0.014,
                [sg * (w / 2.0 + 0.003), 0.0, 0.0],
                Axis::X,
                M::Body2,
                28,
            );
        }
        return Geom {
            fw: w * 0.96,
            fh: r * 1.6,
            z: r * 0.9,
            rnd: false,
        };
    }
    let mut cap = 0.025f64.min(w * 0.04);
    let pl = face_plane(faces);
    if pl.is_some() {
        cap = cap.min(0.007);
    }
    let rec = cl(0.12 * d, 0.004, 0.012);
    let secs = [
        Sec::new(-w / 2.0 + cap, d / 2.0, h / 2.0, 8.0),
        Sec::new(w / 2.0 - cap, d / 2.0, h / 2.0, 8.0),
    ];
    b.loft("bar_body", &secs, M::Body, 28, ry(FRAC_PI_2));
    for sg in [-1.0, 1.0] {
        b.rbox(
            "end_cap",
            [cap * 2.0, h * 1.02, d * 1.02],
            [sg * (w / 2.0 - cap), 0.0, 0.0],
            M::Body2,
            0.18,
            16,
            8,
        );
    }
    let mut fh = h * 0.72;
    let mut fw = w - 2.0 * cap - 0.02;
    let mut t = 0.004f64.max(fh * 0.1);
    if let Some(pl) = pl {
        fw = (pl[0] * 1.005).min(w * 0.995) + 2.0 * t;
        let inner = (pl[1] * 1.04).min(h * 0.97);
        fh = inner + 2.0 * t;
        if fh > h * 0.98 {
            t = 0.002f64.max((h * 0.98 - inner) / 2.0);
            fh = inner + 2.0 * t;
        }
    }
    b.box_(
        "lens_frame_t",
        [fw, t, rec + 0.002],
        [0.0, fh / 2.0 - t / 2.0, d / 2.0],
        M::Body2,
    );
    b.box_(
        "lens_frame_b",
        [fw, t, rec + 0.002],
        [0.0, -fh / 2.0 + t / 2.0, d / 2.0],
        M::Body2,
    );
    for sg in [-1.0, 1.0] {
        b.box_(
            "lens_frame_e",
            [t, fh, rec + 0.002],
            [sg * (fw / 2.0 - t / 2.0), 0.0, d / 2.0],
            M::Body2,
        );
    }
    b.box_(
        "lens_strip",
        [fw - 2.0 * t, fh - 2.0 * t, 0.004],
        [0.0, 0.0, d / 2.0 - rec],
        M::Body2,
    );
    Geom {
        fw: fw - 2.0 * t,
        fh: fh - 2.0 * t,
        z: d / 2.0 - rec,
        rnd: false,
    }
}

/// A single-lens pattern face on a long bar becomes a row of lenses.
fn autofill_bar_layout(faces: &[Face], w: f64, h: f64) -> Cow<'_, [Face]> {
    let Some(z) = faces[0].zones.as_ref().and_then(|z| z.first()) else {
        return Cow::Borrowed(faces);
    };
    let single = z
        .layout
        .as_ref()
        .is_some_and(|l| l.kind.as_deref() == Some("single"));
    if z.emitter.is_some() || !single || w <= 4.0 * h {
        return Cow::Borrowed(faces);
    }
    let mut faces = faces.to_vec();
    let zone = &mut faces[0].zones.as_mut().expect("checked above")[0];
    let lay: &mut ZoneLayout = zone.layout.as_mut().expect("checked above");
    lay.kind = Some("line".into());
    lay.n = Some(int(w / 0.04).min(64).max(4) as f64);
    Cow::Owned(faces)
}

fn dome_face(b: &mut Rig, faces: &[Face], r: f64) -> Res {
    let Some(zone) = faces[0].zones.as_ref().and_then(|z| z.first()) else {
        return fail("dome face without zones");
    };
    let Some(lay) = &zone.layout else {
        return fail("dome zone without a layout");
    };
    let (pts, cw) = face::layout_pts(lay, r * 1.1, r * 1.1, true);
    let nem = pts.len();
    let cells = match zone.cells {
        Some(crate::spec::ZoneCells::Count(c)) if c != 0.0 => int(c),
        _ => nem as i64,
    };
    let nc = cells.min(nem as i64).max(1) as usize;
    let base = b.next_cell;
    b.next_cell += nc as u32;
    for (j, p) in pts.iter().enumerate() {
        let zz = (r * r - p[0] * p[0] - p[1] * p[1]).max(0.0).sqrt() * 0.98;
        let k = base + ((j * nc) / nem) as u32;
        b.emit_cell(
            k,
            [p[0], p[1], zz],
            "circle",
            crate::rig::Size::D(cw * 0.8),
            0.005,
            M::Emitter,
        );
    }
    b.source(
        DVec3::new(0.0, 0.0, r * 0.9),
        (base..base + nc as u32).collect(),
    );
    Ok(())
}

/// `body.lens_d`, `body.frame` and `body.louvres`, and barn doors on boxy heads.
fn head_extras(b: &mut Rig, body: &Body, mut g: Geom, hl: f64) -> Geom {
    if has_feature(body, "barn_doors")
        && (body.shell.is_some() || matches!(body.shape, BodyShape::Box | BodyShape::Panel))
    {
        barn(b, body.w * 0.96, body.d * 0.96, hl / 2.0);
    }
    if let Some(ld) = body.lens_d.filter(|&v| v != 0.0) {
        if matches!(
            body.shape,
            BodyShape::Can | BodyShape::Disc | BodyShape::Box
        ) {
            let d = ld.min(body.w.min(body.d) * 0.98);
            g.fw = d;
            g.fh = d;
        }
    }
    let (fw, fh, z) = (g.fw, g.fh, g.z);
    if let Some(fr) = &body.frame {
        let big =
            fr.r.filter(|&v| v != 0.0)
                .unwrap_or(fw.min(fh) / 2.0 * 1.12);
        let dep = fr.depth.unwrap_or(0.012);
        let n = int(fr.spokes.unwrap_or(4.0));
        bezel_ring(
            b,
            "frame_ring",
            z,
            z + dep,
            big,
            big,
            fr.w.unwrap_or(0.008),
            M::Body2,
            None,
        );
        let a0 = fr.angle.unwrap_or(45.0).to_radians();
        for i in 0..n.max(0) {
            let a = a0 + 2.0 * PI * i as f64 / n as f64;
            let from = DVec3::new(0.0, 0.0, z + dep / 2.0);
            let to = DVec3::new(big * 0.96 * a.cos(), big * 0.96 * a.sin(), z + dep / 2.0);
            b.rod("spoke", from, to, fr.spoke_r.unwrap_or(0.004), M::Body2);
        }
        if n != 0 {
            b.cyl(
                "frame_hub",
                fr.hub.unwrap_or(0.012),
                dep,
                [0.0, 0.0, z + dep / 2.0],
                Axis::Z,
                M::Body2,
                16,
            );
        }
    }
    if let Some(lv) = &body.louvres {
        let n = int(lv.n.unwrap_or(8.0)).max(2);
        let tl = lv.tilt.unwrap_or(30.0).to_radians();
        let dep = lv.depth.unwrap_or(0.02);
        let horizontal = lv.orient.as_deref().unwrap_or("h") == "h";
        for i in 0..n {
            let off = ((i as f64 + 0.5) / n as f64 - 0.5) * if horizontal { fh } else { fw };
            if horizontal {
                b.at(
                    tr(0.0, off, z + dep / 2.0 + 0.002) * rot(tl, DVec3::X),
                    |b| {
                        b.box_("louvre", [fw, 0.003, dep], [0.0; 3], M::Diffuser);
                    },
                );
            } else {
                b.at(
                    tr(off, 0.0, z + dep / 2.0 + 0.002) * rot(tl, DVec3::Y),
                    |b| {
                        b.box_("louvre", [0.003, fh, dep], [0.0; 3], M::Diffuser);
                    },
                );
            }
        }
    }
    g
}

/// Shell preset `name` with `params` as sections (`w`, `d` fractions of the
/// body's width and depth).
fn shell_presets(
    name: &str,
    q: &std::collections::BTreeMap<String, f64>,
) -> Res<Vec<ShellSection>> {
    let g = |k: &str, v: f64| q.get(k).copied().unwrap_or(v);
    let mut r = g("r", 1.0);
    let mut ch = g("chamfer", 0.0);
    let dk = g("dk", 1.0);
    let s = |t: f64, w: f64, rr: f64, c: f64| ShellSection {
        t,
        w,
        d: Some(1.0 - (1.0 - w) * dk),
        r: Some(rr),
        p: None,
        chamfer: Some(c),
    };
    Ok(match name {
        "barrel" => {
            let (rear, fr) = (g("rear", 0.8), g("front", 1.0));
            vec![
                s(0.0, rear * 0.82, r, ch),
                s(0.06, rear, r, ch),
                s(0.2, 1.0, r, ch),
                s(0.9, fr, r, ch),
                s(1.0, fr * 0.98, r, ch),
            ]
        }
        "bowl" => {
            let (rear, fr, bt) = (g("rear", 0.55), g("front", 0.78), g("bulge_t", 0.5));
            linspace(0.0, 1.0, 11)
                .into_iter()
                .map(|t| {
                    let w = if t <= bt {
                        rear + (1.0 - rear) * (FRAC_PI_2 * (t / bt).min(1.0)).sin()
                    } else {
                        1.0 - (1.0 - fr) * ((t - bt) / (1.0 - bt)).powf(1.4)
                    };
                    s(t, w, r, ch)
                })
                .collect()
        }
        "flared" => {
            let (rear, fr, cv) = (g("rear", 0.5), g("front", 1.0), g("curve", 1.6));
            let mut out: Vec<_> = linspace(0.0, 0.94, 9)
                .into_iter()
                .map(|t| s(t, rear + (fr - rear) * t.powf(cv), r, ch))
                .collect();
            out.push(s(1.0, fr, r, ch));
            out
        }
        "tapered_spot" => {
            let (rear, fr, pt) = (g("rear", 0.82), g("front", 0.68), g("peak_t", 0.3));
            vec![
                s(0.0, rear * 0.93, r, ch),
                s(pt * 0.5, rear, r, ch),
                s(pt, 1.0, r, ch),
                s((pt + 1.0) / 2.0, (1.0 + fr) / 2.0 + 0.02, r, ch),
                s(1.0, fr, r, ch),
            ]
        }
        "faceted_box" => {
            let rear = g("rear", 0.72);
            r = g("r", 0.25);
            ch = g("chamfer", 0.5);
            vec![
                s(0.0, rear, r, ch),
                s(0.1, 0.95, r, ch),
                s(0.25, 1.0, r, ch),
                s(0.92, 1.0, r, ch),
                s(1.0, 0.98, r, ch),
            ]
        }
        "drum" => {
            let rear = g("rear", 0.9);
            r = g("r", 0.85);
            vec![
                s(0.0, rear, r, ch),
                s(0.03, 1.0, r, ch),
                s(0.97, 1.0, r, ch),
                s(1.0, 0.95, r, ch),
            ]
        }
        other => return fail(format!("unknown shell preset {other}")),
    })
}

fn head_shell(b: &mut Rig, s: &Body, sh: &Shell, hw: f64, hc: f64, hl: f64) -> Res<Geom> {
    let (rx0, ry0) = (hw / 2.0, hc / 2.0);
    let zf = hl / 2.0;
    let rg = sh.ring.as_ref();
    let rec = rg.and_then(|r| r.rec).unwrap_or_else(|| rec_of(hl));
    let mut secs = match sh.sections.as_ref().filter(|s| !s.is_empty()) {
        Some(s) => s.clone(),
        None => shell_presets(
            sh.preset.as_deref().unwrap_or("barrel"),
            sh.params
                .as_ref()
                .unwrap_or(&std::collections::BTreeMap::new()),
        )?,
    };
    secs.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    let ts_: Vec<f64> = secs.iter().map(|q| q.t).collect();
    let ws: Vec<f64> = secs.iter().map(|q| q.w).collect();
    let ds: Vec<f64> = secs.iter().map(|q| q.d.unwrap_or(q.w)).collect();
    let rs: Vec<f64> = secs
        .iter()
        .map(|q| match (q.r, q.p) {
            (Some(r), _) => r,
            (None, Some(p)) => cl(1.0 - (p - 2.0) / 14.0, 0.0, 1.0).sqrt(),
            (None, None) => 1.0,
        })
        .collect();
    let chs: Vec<f64> = secs.iter().map(|q| q.chamfer.unwrap_or(0.0)).collect();
    let f = |t: f64| {
        (
            interp(t, &ts_, &ws),
            interp(t, &ts_, &ds),
            interp(t, &ts_, &rs),
            interp(t, &ts_, &chs),
        )
    };
    let te = 1.0 - rec / hl;
    let mut ts: Vec<f64> = ts_.iter().copied().filter(|&t| t < te - 1e-4).collect();
    ts.push(te);
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ts.dedup();
    let loft: Vec<Sec> = ts
        .iter()
        .map(|&t| {
            let (w, d, r, ch) = f(t);
            Sec::new(-hl / 2.0 + t * hl, rx0 * w, ry0 * d, rp(r)).ch(ch)
        })
        .collect();
    b.loft("shell_body", &loft, M::Body, 44, DMat4::IDENTITY);
    let (w, d, r, ch) = f(te);
    let (rxf, ryf) = (rx0 * w, ry0 * d);
    let wall = rg
        .and_then(|r| r.wall)
        .unwrap_or(0.006f64.max(rxf.min(ryf) * 0.1));
    if rg.and_then(|r| r.bezel).unwrap_or(true) {
        let sc = rg.and_then(|r| r.scale).unwrap_or(1.0);
        let (z0, z1) = (
            zf - rec,
            zf + rg.and_then(|r| r.proud).unwrap_or(hl * 0.008),
        );
        let m = mesh::ring(
            rxf * sc,
            ryf * sc,
            rxf * sc - wall,
            ryf * sc - wall,
            z0,
            z1,
            rp(r),
            ch,
            44,
        );
        b.add("bezel", m, M::Body2, DMat4::IDENTITY);
    }
    if let Some(rc) = sh
        .rear_cap
        .as_ref()
        .filter(|c| c.r.is_some() || c.len.is_some())
    {
        let len = rc.len.unwrap_or(hl * 0.04);
        let rad = rx0.min(ry0) * rc.r.unwrap_or(0.5) * f(0.0).0;
        b.cyl(
            "rear_cap",
            rad,
            len,
            [0.0, 0.0, -hl / 2.0 - len / 2.0 + 0.002],
            Axis::Z,
            M::Body2,
            28,
        );
    }
    if let Some(fins) = &sh.fins {
        let n = int(fins.n.unwrap_or(5.0));
        let h = fins.h.unwrap_or(0.012);
        let (t0, t1) = (fins.t0.unwrap_or(0.1), fins.t1.unwrap_or(0.5));
        for i in 0..n.max(0) {
            let t = t0 + (t1 - t0) * (i as f64 / (n - 1).max(1) as f64);
            let (w_, d_, r_, c_) = f(t);
            let z = -hl / 2.0 + t * hl;
            let th = 0.004f64.max(hl * 0.012);
            let secs = [
                Sec::new(z - th / 2.0, rx0 * w_ + h, ry0 * d_ + h, rp(r_)).ch(c_),
                Sec::new(z + th / 2.0, rx0 * w_ + h, ry0 * d_ + h, rp(r_)).ch(c_),
            ];
            b.loft("fin", &secs, M::Body2, 36, DMat4::IDENTITY);
        }
    }
    if sh.pods.unwrap_or(true) {
        let off = s.off.unwrap_or(0.0);
        let wa = f(cl((off + hl / 2.0) / hl, 0.0, 1.0)).0;
        for sg in [-1.0, 1.0] {
            b.cyl(
                "pod",
                rx0.min(ry0) * 0.28,
                0.008,
                [sg * (rx0 * wa + 0.001), 0.0, -off],
                Axis::X,
                M::Body2,
                20,
            );
        }
    }
    if ch >= 0.3 || (r < 0.3 && ch > 0.0) {
        let q = 2.0 * (rxf.min(ryf) - wall) * 0.9;
        return Ok(Geom {
            fw: q,
            fh: q,
            z: zf - rec,
            rnd: true,
        });
    }
    let k = if r >= 0.7 {
        0.94
    } else {
        0.94f64.min(inscribed(rp(r), ch) * 0.97)
    };
    Ok(Geom {
        fw: 2.0 * (rxf - wall) * k,
        fh: 2.0 * (ryf - wall) * k,
        z: zf - rec,
        rnd: r >= 0.7,
    })
}

/// A spec joint as built. Its rest is 0: only a luminaire chain's tilt sets
/// one (the reference builder poses nothing else at the spec's `rest`).
pub(crate) fn joint_rec(kind: JointKind, default_axis: DVec3, j: &Joint) -> JointRec {
    JointRec {
        kind,
        axis: j.axis.map_or(default_axis, DVec3::from),
        range: j.range,
        rest: 0.0,
        drive: j.drive.unwrap_or(crate::spec::Drive::Dmx),
        per_head: j.per_head.unwrap_or(false),
    }
}

/// The head frame's content: body, faces, and the spin (face) and slide
/// (nose) joints. `spin`/`slide` are the joints this head carries.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_head(
    b: &mut Rig,
    body: &Body,
    faces: &[Face],
    spin: &[Joint],
    slide: &[Joint],
    hw: f64,
    hc: f64,
    hl: f64,
) -> Res {
    if faces.is_empty() {
        return fail("fixture without faces");
    }
    let shape = body.shape;
    let faces = if shape == BodyShape::Bar {
        autofill_bar_layout(faces, hw, hc)
    } else {
        Cow::Borrowed(faces)
    };
    if shape == BodyShape::Dome {
        body_dome(b, body, hw, hl);
        let r = hw / 2.0;
        b.lens(-hl / 2.0 + r, [hw, hw], true);
        return b.at(tr(0.0, 0.0, -hl / 2.0), |b| dome_face(b, &faces, r));
    }
    let shell = body
        .shell
        .as_ref()
        .filter(|_| matches!(shape, BodyShape::Can | BodyShape::Box));
    let g = match (shell, shape) {
        (Some(sh), _) => head_shell(b, body, sh, hw, hc, hl)?,
        (None, BodyShape::Can) => body_can(b, body, hw, hc, hl)?,
        (None, BodyShape::Box) => body_box(b, body, hw, hc, hl, &faces),
        (None, BodyShape::Bar) => body_bar(b, body, hw, hc, hl, &faces),
        (None, BodyShape::Panel) => body_panel(b, body, hw, hc, hl),
        (None, BodyShape::Disc) => body_disc(b, hw, hc, hl),
        (None, BodyShape::Dome) => unreachable!("handled above"),
    };
    let mut g = head_extras(b, body, g, hl);
    let zoned =
        faces[0].root.is_some() || faces[0].zones.iter().flatten().any(|z| z.emitter.is_some());
    if shape == BodyShape::Bar && zoned && !g.rnd {
        g.z = hl / 2.0 - 0.0005;
    }
    let pl = face_plane(&faces);
    if let Some(pl) = pl {
        if matches!(shape, BodyShape::Bar | BodyShape::Box | BodyShape::Panel) {
            g.fw = (pl[0] * 1.005).min(hw * 0.995);
            g.fh = (pl[1] * 1.04).min(hc * 0.97);
        }
    }
    if shape == BodyShape::Box && pl.is_none() && zoned {
        g.fw = hw * 0.87;
        g.fh = hc * 0.86;
    }
    if matches!(shape, BodyShape::Box | BodyShape::Panel) && body.shell.is_none() {
        if g.rnd {
            let q = g.fw.min(g.fh).min(0.9 * hw.min(hc));
            g.fw = q;
            g.fh = q;
        } else {
            let p = body
                .corner
                .map_or(if shape == BodyShape::Box { 4.5 } else { 6.0 }, rp);
            let kk = inscribed(p, 0.0) * 0.97;
            g.fw = g.fw.min(hw * kk);
            g.fh = g.fh.min(hc * kk);
        }
    }
    let faces = &faces;
    let with_spin = |b: &mut Rig| -> Res {
        let inner = |b: &mut Rig| -> Res {
            b.lens(g.z, [g.fw, g.fh], g.rnd);
            face::faces_on(b, faces, g)
        };
        match spin.first() {
            Some(j) => b.joint(
                "spin_0",
                Some(joint_rec(JointKind::Spin, DVec3::Z, j)),
                None,
                None,
                inner,
            ),
            None => inner(b),
        }
    };
    match slide.first().filter(|_| shape == BodyShape::Can) {
        Some(j) => b.joint(
            "slide_0",
            Some(joint_rec(JointKind::Slide, DVec3::Z, j)),
            None,
            None,
            |b| {
                b.cyl(
                    "zoom_barrel",
                    hw.min(hc) / 2.0 * 0.62,
                    hl * 0.12,
                    [0.0, 0.0, g.z + hl * 0.02],
                    Axis::Z,
                    M::Body2,
                    28,
                );
                with_spin(b)
            },
        ),
        None => with_spin(b),
    }
}
