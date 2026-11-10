//! Rigs: the luminaire chain (mount, pan, yoke, tilt, head), the deflector's
//! moving element, and arrays of heads on a carrier. Fixture coordinates at
//! rest: metres, Y up, front +Z, origin at the floor centre.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use glam::{DMat4, DVec3};

use crate::head::{build_head, joint_rec, rp};
use crate::mesh::{self, interp, linspace, Mesh, Sec};
use crate::rig::{cl, fail, int, rot, rx, tr, Axis, JointRec, Res, Rig};
use crate::spec::{
    Arm, ArmProfile, Attach, Base, BaseShape, Body, BodyShape, CarrierKind, Clamp,
    DeflectorElement, Face, Fixture, Hull, Joint, MaterialSlot as M, Mount, MountType, PartShape,
    Template, Yoke, YokeShape,
};
use crate::JointKind;

/// What one luminaire chain is built from: the fixture's own rig, or one
/// array element's.
pub(crate) struct Chain<'a> {
    pub size: [f64; 3],
    pub template: Template,
    pub mount: &'a Mount,
    pub body: &'a Body,
    pub faces: &'a [Face],
    pub pan: Option<&'a Joint>,
    pub tilt: Option<&'a Joint>,
    pub spin: &'a [Joint],
    pub slide: &'a [Joint],
    /// The fixture, for its `rig.parts` and `deflector`; `None` for an array element.
    pub fixture: Option<&'a Fixture>,
    pub names: [String; 3],
    pub element: Option<usize>,
}

fn tripod(b: &mut Rig, top: f64, legs: bool) {
    if legs {
        for i in 0..3 {
            let a = i as f64 * TAU / 3.0 + FRAC_PI_2;
            b.rod(
                "leg",
                DVec3::new(0.0, top, 0.0),
                DVec3::new(0.35 * a.cos(), 0.0, 0.35 * a.sin()),
                0.01,
                M::Body2,
            );
        }
    } else {
        b.cyl(
            "stand_foot",
            0.15,
            0.02,
            [0.0, 0.01, 0.0],
            Axis::Y,
            M::Body2,
            28,
        );
    }
    b.cyl(
        "pole",
        0.012,
        top,
        [0.0, top / 2.0, 0.0],
        Axis::Y,
        M::Metal,
        28,
    );
}

/// One yoke arm: a slab of thickness `t` whose depth tapers from `yd` at `y0`,
/// with a round top around the tilt axis.
#[allow(clippy::too_many_arguments)]
fn yoke_arm(
    b: &mut Rig,
    name: &'static str,
    top: &'static str,
    x: f64,
    t: f64,
    yd: f64,
    y0: f64,
    y1: f64,
    taper: &[f64],
) {
    let xp = linspace(0.0, 1.0, taper.len());
    let ds = |u: f64| yd * interp(u, &xp, taper);
    let secs: Vec<Sec> = linspace(y0, y1 - ds(1.0) / 2.0, 7)
        .into_iter()
        .map(|y| Sec::new(y, t / 2.0, ds((y - y0) / (y1 - y0).max(1e-6)) / 2.0, 5.0))
        .collect();
    b.loft_y(name, &secs, M::Body2, 20, [x, 0.0, 0.0]);
    b.cyl(
        top,
        ds(1.0) / 2.0,
        t,
        [x, y1 - ds(1.0) / 2.0, 0.0],
        Axis::X,
        M::Body2,
        20,
    );
}

/// `yoke.arm`: a shaped arm plate (FORMAT.md, "Yoke arms").
#[allow(clippy::too_many_arguments)]
fn yoke_arm2(b: &mut Rig, x: f64, sg: f64, arm: &Arm, y0: f64, y1: f64, t0: f64, yd0: f64) -> Res {
    let pf = arm.profile.unwrap_or(ArmProfile::Tapered);
    let tb: Vec<f64> = match &arm.t {
        Some(crate::spec::NumOrList::List(l)) if !l.is_empty() => l.clone(),
        Some(crate::spec::NumOrList::Num(v)) if *v != 0.0 => vec![*v, *v],
        _ => vec![t0, t0],
    };
    let db: Vec<f64> = match arm.d.as_ref().filter(|d| !d.is_empty()) {
        Some(d) => d.clone(),
        None => match pf {
            ArmProfile::Straight => vec![yd0, yd0],
            ArmProfile::Tapered => vec![yd0, yd0 * 0.65],
            ArmProfile::Curved => vec![yd0, yd0 * 0.6],
            ArmProfile::Cheek => vec![yd0 * 1.25, yd0 * 0.55],
        },
    };
    if tb.len() < 2 || db.len() < 2 {
        return fail("yoke arm t/d is not [bottom, top]");
    }
    let bow = arm.bow.unwrap_or(if pf == ArmProfile::Curved {
        0.3 * yd0
    } else {
        0.0
    });
    let lean = arm.lean.unwrap_or(0.0);
    let rt = arm
        .round_top
        .unwrap_or(if pf == ArmProfile::Cheek { 0.35 } else { 0.5 });
    let p = rp(rt * 0.9);
    let dt = db[1];
    let ytop = y1 - dt / 2.0;
    let sec = |u: f64| {
        let t = tb[0] + (tb[1] - tb[0]) * u;
        let d = db[0] + (db[1] - db[0]) * u;
        Sec::new(y0 + (ytop - y0) * u, t / 2.0, d / 2.0, p)
            .at(sg * lean * (1.0 - u), -(bow * (PI * u).sin() * 0.5))
    };
    let us =
        |lo: f64, hi: f64, n: usize| (0..=n).map(move |i| lo + (hi - lo) * i as f64 / n as f64);
    let at = [x, 0.0, 0.0];
    if let Some(co) = &arm.cutout {
        let a = cl(co.y0.unwrap_or(0.3), 0.05, 0.9);
        let c = cl(co.y1.unwrap_or(0.7), 0.1, 0.95);
        let k = 0.006f64.max(co.strut.unwrap_or(0.02));
        b.loft_y(
            "yoke_arm_lo",
            &us(0.0, a, 3).map(sec).collect::<Vec<_>>(),
            M::Body2,
            20,
            at,
        );
        b.loft_y(
            "yoke_arm_hi",
            &us(c, 1.0, 4).map(sec).collect::<Vec<_>>(),
            M::Body2,
            20,
            at,
        );
        for sgn in [-1.0, 1.0] {
            let ss: Vec<Sec> = us(a, c, 3)
                .map(|u| {
                    let mut q = sec(u);
                    let d = db[0] + (db[1] - db[0]) * u;
                    q.ry = k / 2.0;
                    q.cy += sgn * (d / 2.0 - k / 2.0);
                    q
                })
                .collect();
            b.loft_y("yoke_arm_strut", &ss, M::Body2, 16, at);
        }
    } else {
        b.loft_y(
            "yoke_arm",
            &(0..9).map(|i| sec(i as f64 / 8.0)).collect::<Vec<_>>(),
            M::Body2,
            20,
            at,
        );
    }
    if rt >= 0.3 {
        b.cyl(
            "yoke_arm_top",
            dt / 2.0,
            tb[1],
            [x, ytop, 0.0],
            Axis::X,
            M::Body2,
            20,
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn yoke_shape(b: &mut Rig, yk: &Yoke, xs: &[f64], t: f64, yd: f64, by: f64, ty: f64, bottom: f64) {
    let xo = xs.iter().map(|x| x.abs()).fold(0.0, f64::max);
    if let Some(bar) = yk.bar.as_ref().filter(|_| xs.len() >= 2) {
        let size = [2.0 * xo + t, bar.h.unwrap_or(t * 1.2), bar.d.unwrap_or(yd)];
        b.rbox(
            "yoke_bar",
            size,
            [0.0, bar.y.unwrap_or(by + 0.02), 0.0],
            M::Body2,
            0.3,
            24,
            14,
        );
    }
    let shape = yk.shape.unwrap_or(YokeShape::Arm);
    if shape == YokeShape::Arm || xs.len() < 2 {
        return;
    }
    if shape == YokeShape::U {
        let yb = yk
            .bar_y
            .unwrap_or((by + 0.01).max(bottom - 0.012 - t * 0.6));
        b.rbox(
            "yoke_u_bar",
            [2.0 * xo + t, t * 1.2, yd],
            [0.0, yb, 0.0],
            M::Body2,
            0.3,
            24,
            14,
        );
        if yb - by > 0.02 {
            b.box_(
                "yoke_stem",
                [(xo * 0.8).min(0.08), yb - by, yd * 0.8],
                [0.0, (yb + by) / 2.0, 0.0],
                M::Body2,
            );
        }
    } else {
        let r = xo;
        let rr = 0.005f64.max(t.min(yd) * 0.45);
        let pts: Vec<DVec3> = linspace(PI, TAU, 14)
            .into_iter()
            .map(|a| DVec3::new(r * a.cos(), ty + r * a.sin(), 0.0))
            .collect();
        for w in pts.windows(2) {
            b.rod("yoke_arch", w[0], w[1], rr, M::Body2);
        }
        if ty - r - by > 0.02 {
            b.box_(
                "yoke_stem",
                [(r * 0.8).min(0.08), ty - r - by, yd * 0.8],
                [0.0, (ty - r + by) / 2.0, 0.0],
                M::Body2,
            );
        }
    }
}

fn build_feet(b: &mut Rig, bs: &Base) {
    let Some(ft) = &bs.feet else { return };
    let n = int(ft.n.unwrap_or(2.0));
    let (fw, fh, fd) = (
        ft.w.unwrap_or(0.05),
        ft.h.unwrap_or(0.02),
        ft.d.unwrap_or(bs.d),
    );
    let out = ft.out.unwrap_or(fw * 0.3);
    let zs: &[f64] = if n <= 2 { &[0.0] } else { &[-1.0, 1.0] };
    for sx in [-1.0, 1.0] {
        for &sz in zs {
            let z = if sz == 0.0 {
                0.0
            } else {
                sz * (bs.d / 2.0 - fd / 2.0)
            };
            b.rbox(
                "foot",
                [fw, fh, fd],
                [sx * (bs.w / 2.0 - fw / 2.0 + out), fh / 2.0, z],
                M::Body2,
                0.3,
                24,
                14,
            );
        }
    }
}

fn base_details(b: &mut Rig, bs: &Base, y_mid: f64, handles: bool, display: bool) {
    let (w, h, d) = (bs.w, bs.h, bs.d);
    let zf = d / 2.0;
    if display {
        let (dw, dh) = (w * 0.34, h * 0.32);
        b.box_(
            "display_bezel",
            [dw + 0.008, dh + 0.008, 0.004],
            [0.0, y_mid, zf + 0.001],
            M::Body2,
        );
        b.box_(
            "base_display",
            [dw, dh, 0.004],
            [0.0, y_mid, zf + 0.003],
            M::Lens,
        );
    }
    if handles {
        for sg in [-1.0, 1.0] {
            b.box_(
                "handle_recess",
                [0.006, h * 0.3, d * 0.34],
                [sg * (w / 2.0 + 0.001), y_mid, 0.0],
                M::Body2,
            );
            b.box_(
                "handle_bar",
                [0.014, h * 0.12, d * 0.3],
                [sg * (w / 2.0 + 0.008), y_mid, 0.0],
                M::Metal,
            );
        }
    }
}

fn build_base(b: &mut Rig, bs: &Base) {
    build_feet(b, bs);
    let (w, h, d) = (bs.w, bs.h, bs.d);
    let tiers = bs.tiers.as_ref().filter(|t| !t.is_empty());
    match (bs.shape, tiers) {
        (BaseShape::Disc, _) => {
            let r = w.min(d) / 2.0;
            let secs = [
                Sec::new(0.0, r * 0.92, r * 0.92, 2.0),
                Sec::new(h * 0.12, r, r, 2.0),
                Sec::new(h * 0.7, r, r, 2.0),
                Sec::new(h, r * 0.8, r * 0.8, 2.0),
            ];
            b.loft_y("base", &secs, M::Body, 36, [0.0; 3]);
        }
        (BaseShape::U, _) => {
            let bw = w * cl(bs.block.unwrap_or(0.28), 0.1, 0.45);
            for sg in [-1.0, 1.0] {
                b.rbox(
                    "base_block",
                    [bw, h, d],
                    [sg * (w / 2.0 - bw / 2.0), h / 2.0, 0.0],
                    M::Body,
                    0.3,
                    24,
                    14,
                );
            }
            b.rbox(
                "base_bridge",
                [w - bw, h * 0.45, d],
                [0.0, h * 0.225, 0.0],
                M::Body,
                0.3,
                24,
                14,
            );
        }
        (BaseShape::Box, Some(tiers)) => {
            let tot: f64 = tiers.iter().map(|t| t.h.unwrap_or(1.0)).sum();
            let mut y = 0.0;
            for t in tiers {
                let th = h * t.h.unwrap_or(1.0) / tot;
                let p = rp(t.r.unwrap_or(0.6));
                let ch = t.chamfer.unwrap_or(0.0);
                let (rx, rz) = (w * t.w.unwrap_or(1.0) / 2.0, d * t.d.unwrap_or(1.0) / 2.0);
                let (x0, z0) = (t.x.unwrap_or(0.0) * w, t.z.unwrap_or(0.0) * d);
                let sec = |y: f64, k: f64| Sec::new(y, rx * k, rz * k, p).ch(ch).at(x0, -z0);
                let secs = [
                    sec(y, 0.97),
                    sec(y + th * 0.12, 1.0),
                    sec(y + th * 0.88, 1.0),
                    sec(y + th, 0.97),
                ];
                b.loft_y("base_tier", &secs, t.mat.unwrap_or(M::Body), 32, [0.0; 3]);
                y += th;
            }
            base_details(
                b,
                bs,
                h * 0.42,
                bs.handles.unwrap_or(false),
                bs.display.unwrap_or(false),
            );
        }
        (BaseShape::Box, None) => {
            let secs = [
                Sec::new(0.0, w * 0.46, d * 0.46, 6.0),
                Sec::new(h * 0.1, w * 0.5, d * 0.5, 6.0),
                Sec::new(h * 0.68, w * 0.5, d * 0.5, 6.0),
                Sec::new(h * 0.86, w * 0.46, d * 0.46, 6.0),
                Sec::new(h, w * 0.4, d * 0.4, 6.0),
            ];
            b.loft_y("base", &secs, M::Body, 32, [0.0; 3]);
            base_details(
                b,
                bs,
                h * 0.42,
                bs.handles.unwrap_or(true),
                bs.display.unwrap_or(true),
            );
        }
    }
}

/// `rig.parts[]` attached to `attach`, in the frame they move with.
fn add_parts(b: &mut Rig, f: Option<&Fixture>, attach: Attach) -> Res {
    let Some(parts) = f.and_then(|f| f.rig.parts.as_ref()) else {
        return Ok(());
    };
    for (i, q) in parts.iter().enumerate() {
        if q.attach.unwrap_or(Attach::Head) != attach {
            continue;
        }
        let pos = q.pos.unwrap_or([0.0; 3]);
        let [ax, ay, az] = q.rot.unwrap_or([0.0; 3]).map(f64::to_radians);
        let m =
            tr(pos[0], pos[1], pos[2]) * rot(az, DVec3::Z) * rot(ay, DVec3::Y) * rot(ax, DVec3::X);
        let need = |v: Option<f64>, what: &str| {
            v.ok_or_else(|| crate::rig::BuildError(format!("part {i} needs {what}")))
        };
        let mesh: Mesh = match q.shape.unwrap_or(PartShape::Box) {
            PartShape::Box => {
                let Some(&[x, y, z]) = q.size.as_deref() else {
                    return fail(format!("part {i} needs size [x, y, z]"));
                };
                mesh::cuboid(DVec3::new(x, y, z).max(DVec3::splat(1e-4)))
            }
            PartShape::Cyl => {
                mesh::cylinder(need(q.r, "r")?.max(1e-4), need(q.len, "len")?.max(1e-4), 24)
            }
            PartShape::Sphere => mesh::uv_sphere(need(q.r, "r")?.max(1e-4), [12, 16]),
            PartShape::Cone => {
                mesh::cone(need(q.r, "r")?.max(1e-4), need(q.len, "len")?.max(1e-4), 24)
            }
            PartShape::Loft => {
                let Some(secs) = q.sections.as_ref().filter(|s| !s.is_empty()) else {
                    return fail(format!("part {i} needs sections"));
                };
                let secs: Vec<Sec> = secs
                    .iter()
                    .map(|&[z, rx, ry, r]| Sec::new(z, rx, ry, rp(r)))
                    .collect();
                mesh::loft(&secs, 36)
            }
            PartShape::Grille => {
                let Some(&[w, h, t]) = q.size.as_deref() else {
                    return fail(format!("part {i} needs size [w, h, t]"));
                };
                let [nx, ny] = q.bars.unwrap_or([6.0, 4.0]).map(int);
                let bw = q.bar.unwrap_or(w.min(h) * 0.03);
                let mut m = Mesh::default();
                for k in 0..=nx {
                    let mut bar = mesh::cuboid(DVec3::new(bw, h, t));
                    bar.transform(&tr(-w / 2.0 + k as f64 * w / nx.max(1) as f64, 0.0, 0.0));
                    m.append(&bar);
                }
                for k in 0..=ny {
                    let mut bar = mesh::cuboid(DVec3::new(w, bw, t));
                    bar.transform(&tr(0.0, -h / 2.0 + k as f64 * h / ny.max(1) as f64, 0.0));
                    m.append(&bar);
                }
                m
            }
            PartShape::Torus => mesh::torus(need(q.r, "r")?, q.t.unwrap_or(0.01), 32, 10),
        };
        let name = q
            .name
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("part{i}"));
        b.add_part(&name, mesh, q.mat.unwrap_or(M::Body2), m);
    }
    Ok(())
}

/// The whole chain, upside down from the top of `size[1]` when the mount hangs.
pub(crate) fn build_luminaire(b: &mut Rig, c: &Chain) -> Res {
    if c.mount.hang.unwrap_or(false) {
        return b.at(tr(0.0, c.size[1], 0.0) * rot(PI, DVec3::Z), |b| {
            luminaire(b, c)
        });
    }
    luminaire(b, c)
}

fn clamps(b: &mut Rig, cl_: &Clamp, yb: f64, hw: f64, hd: f64) {
    let n = int(cl_.n.unwrap_or(2.0));
    let stem = cl_.stem.unwrap_or(0.03);
    let cw = cl_.w.unwrap_or(0.05);
    let r = cl_.r.unwrap_or(0.025);
    let sp = cl_.spread.unwrap_or(hw * 0.34);
    let xs: Vec<f64> = if n == 1 {
        vec![0.0]
    } else {
        (0..n.max(0))
            .map(|i| (-1.0 + 2.0 * i as f64 / (n - 1) as f64) * sp)
            .collect()
    };
    for x in xs {
        b.box_(
            "clamp_plate",
            [cw, 0.006, hd.max(0.03).min(0.1)],
            [x, yb - 0.003, 0.0],
            M::Body2,
        );
        b.box_(
            "clamp_stem",
            [cw * 0.4, stem, hd.max(0.03).min(0.06)],
            [x, yb - stem / 2.0, 0.0],
            M::Body2,
        );
        let ring = mesh::annulus((r - 0.006).max(1e-3), r, cw, 24);
        b.add(
            "clamp_omega",
            ring,
            M::Metal,
            tr(x, yb - stem - r + 0.006, 0.0) * rot(FRAC_PI_2, DVec3::Y),
        );
    }
}

fn luminaire(b: &mut Rig, c: &Chain) -> Res {
    let (m, body) = (c.mount, c.body);
    let deflector = c.template == Template::Deflector;
    let (pan, tilt) = if deflector {
        (None, None)
    } else {
        (c.pan, c.tilt)
    };
    let wtot = c.size[0];
    let (Some(off), Some(tilt_y)) = (body.off, body.tilt_y) else {
        return fail("a luminaire body needs off and tilt_y");
    };
    let (hw, hd, hl) = (body.w, body.d, body.len);
    let mut ty = tilt_y;
    let mt = m.kind;
    let bs = m.base.as_ref();
    let mut by = 0.0;
    if body.shape == BodyShape::Bar
        && bs.is_none()
        && c.template == Template::Luminaire
        && mt != MountType::Stand
        && hw > 0.3
        && hw > 3.0 * hd
        && hl > 0.06
    {
        // A bar hung under a back rail with two clamp brackets.
        let yb = tilt_y + off - hl / 2.0;
        let rd = (hd * 0.5).max(0.03).min(0.07);
        let pr = 0.022f64.min(0.12 * hl);
        b.box_(
            "mount_rail",
            [hw * 0.86, pr * 0.5, rd],
            [0.0, yb - pr * 0.15, 0.0],
            M::Body2,
        );
        if matches!(mt, MountType::Yoke2 | MountType::Yoke1) {
            b.box_(
                "pan_block",
                [0.07, pr * 0.8, rd],
                [0.0, yb - pr * 0.3, 0.0],
                M::Body2,
            );
        }
        for sx in [-1.0, 1.0] {
            b.box_(
                "clamp_block",
                [0.05, pr, (hd * 0.75).max(0.04).min(0.1)],
                [sx * hw * 0.34, yb - pr * 0.3, 0.0],
                M::Body2,
            );
        }
    }
    if let Some(cl_) = m.clamp.as_ref().filter(|_| body.shape == BodyShape::Bar) {
        clamps(b, cl_, tilt_y + off - hl / 2.0, hw, hd);
    }
    if let Some(bs) = bs {
        by = bs.h;
        build_base(b, bs);
    }
    add_parts(b, c.fixture, Attach::Base)?;
    let bottom = ty + off - hl / 2.0;
    let mut pan_body = |b: &mut Rig| -> Res {
        add_parts(b, c.fixture, Attach::Yoke)?;
        let yokes = matches!(
            mt,
            MountType::Yoke2 | MountType::Yoke1 | MountType::Bail | MountType::Bracket
        );
        match (m.yoke.as_ref().filter(|_| yokes), mt) {
            (Some(yk), _) => yoke(b, c, yk, by, ty, bottom, wtot)?,
            (None, MountType::Stand) => {
                let top = 0.3f64.max((bottom - 0.03).min(1.6));
                tripod(b, top, m.legs.unwrap_or(0.0) == 3.0);
                b.box_(
                    "stand_head",
                    [0.06, 0.03, 0.06],
                    [0.0, top + 0.015, 0.0],
                    M::Body2,
                );
                ty = top + 0.03 + hl / 2.0 - off;
            }
            (None, _) => ty = hl / 2.0 - off + by,
        }
        let head = |b: &mut Rig| -> Res {
            // Built at tilt 0, beam along the pan axis; the joint's rest is the
            // spec's `rest` (see `crate::Joint::rest`).
            b.at(tr(0.0, ty, 0.0) * rx(-FRAC_PI_2) * tr(0.0, 0.0, off), |b| {
                b.joint(&c.names[2], None, c.element, None, |b| {
                    build_head(b, body, c.faces, c.spin, c.slide, hw, hd, hl)?;
                    add_parts(b, c.fixture, Attach::Head)?;
                    if deflector {
                        deflector_element(
                            b,
                            c.fixture.expect("a deflector is a whole fixture"),
                            hw,
                            hd,
                            hl,
                        )?;
                    }
                    Ok(())
                })
            })
        };
        match tilt {
            Some(j) => b.joint(
                &c.names[1],
                Some(JointRec {
                    rest: j.rest.unwrap_or(0.0),
                    ..joint_rec(JointKind::Tilt, DVec3::X, j)
                }),
                c.element,
                Some(DVec3::new(0.0, ty, 0.0)),
                head,
            ),
            None => head(b),
        }
    };
    match pan {
        Some(j) => b.joint(
            &c.names[0],
            Some(joint_rec(JointKind::Pan, DVec3::Y, j)),
            c.element,
            Some(DVec3::new(0.0, by, 0.0)),
            pan_body,
        ),
        None => pan_body(b),
    }
}

#[allow(clippy::too_many_arguments)]
fn yoke(b: &mut Rig, c: &Chain, yk: &Yoke, by: f64, ty: f64, bottom: f64, wtot: f64) -> Res {
    let (m, body) = (c.mount, c.body);
    let (hw, hd) = (body.w, body.d);
    let gap = yk.gap.unwrap_or(0.008);
    let arm = yk.arm.as_ref().filter(|a| {
        a.profile.is_some()
            || a.t.is_some()
            || a.d.is_some()
            || a.bow.is_some()
            || a.lean.is_some()
            || a.round_top.is_some()
            || a.cutout.is_some()
    });
    let yw = yk.w.max(hw + 2.0 * (gap + 0.012));
    let yd = 0.03f64.max(yk.d.min(hd.max(0.05)));
    let y1 = yk.y1.max(ty + 0.02);
    let single = m.kind == MountType::Yoke1;
    let arm_t = arm.and_then(|a| match &a.t {
        Some(crate::spec::NumOrList::List(l)) if !l.is_empty() => {
            Some(l.iter().copied().fold(f64::MIN, f64::max))
        }
        Some(crate::spec::NumOrList::Num(v)) if *v != 0.0 => Some(*v),
        _ => None,
    });
    let xs_of = |t: f64| -> Vec<f64> {
        let x = hw / 2.0 + gap + t / 2.0;
        if single {
            vec![x]
        } else {
            vec![-x, x]
        }
    };
    let bar_span = |t: f64| {
        let wbar = if single {
            hw / 2.0 + gap + t * 1.5
        } else {
            hw + 2.0 * gap + 2.0 * t
        };
        let xc = if single {
            hw / 2.0 + gap + t / 2.0 + t / 2.0 - wbar / 2.0
        } else {
            0.0
        };
        (wbar, xc)
    };
    if let Some(bs) = m.base.as_ref() {
        let mut t = cl((yw - hw) / 2.0 - gap, 0.012, 0.07)
            .min(wtot / 2.0 - hw / 2.0 - gap - 0.003)
            .max(0.004);
        if let Some(at) = arm_t {
            t = 0.004f64.max(at.min(wtot / 2.0 - hw / 2.0 - gap));
        }
        let xs = xs_of(t);
        for &x in &xs {
            let sgn = if x > 0.0 { 1.0 } else { -1.0 };
            match arm {
                Some(arm) => yoke_arm2(b, x, sgn, arm, by, y1, t, yd.max(0.03))?,
                None => yoke_arm(
                    b,
                    "yoke_arm",
                    "yoke_arm_top",
                    x,
                    t,
                    yd.max(0.03),
                    by,
                    y1,
                    &[1.0, 0.95, 0.88, 0.8, 0.74, 0.72],
                ),
            }
            b.cyl(
                "trunnion",
                yd.min(hd) * 0.2,
                t * 0.3,
                [x + sgn * t * 0.5, ty, 0.0],
                Axis::X,
                M::Metal,
                16,
            );
        }
        yoke_shape(b, yk, &xs, t, yd.max(0.03), by, ty, bottom);
        let avail = 0.01f64.max(bottom - by - 0.004);
        let mut bh = cl(0.45 * (bottom - by), 0.015, 0.09).min(avail);
        let (mut wbar, xc) = bar_span(t);
        let hull = match &yk.hull {
            Some(Hull::Flag(true)) => return fail("yoke.hull is true, not a shape"),
            Some(Hull::Shape(h)) => Some(h),
            Some(Hull::Flag(false)) | None => None,
        };
        let hull_off = yk.hull == Some(Hull::Flag(false));
        if let Some(h) = hull.and_then(|h| h.h).filter(|&v| v != 0.0) {
            bh = h;
        }
        if let Some(w) = hull.and_then(|h| h.w).filter(|&v| v != 0.0) {
            wbar = w;
        }
        let hull_d = hull.and_then(|h| h.d).unwrap_or(yd * 1.05);
        if let Some(chamfer) = hull.and_then(|h| h.chamfer).filter(|&v| v != 0.0) {
            let p = rp(hull.and_then(|h| h.r).unwrap_or(0.3));
            let secs = [
                Sec::new(by, wbar / 2.0 * 0.8, hull_d / 2.0, p)
                    .ch(chamfer)
                    .at(xc, 0.0),
                Sec::new(by + bh, wbar / 2.0, hull_d / 2.0, p)
                    .ch(chamfer)
                    .at(xc, 0.0),
            ];
            b.loft_y("yoke_hull", &secs, M::Body2, 32, [0.0; 3]);
        } else if !hull_off && bs.shape == BaseShape::Disc {
            let r = bs.w.min(bs.d) / 2.0;
            let (hx, hz) = ((wbar / 2.0).min(r * 0.98), (hull_d / 2.0).min(r * 0.9));
            let secs = [
                Sec::new(by, hx * 0.92, hz * 0.92, 3.0),
                Sec::new(by + bh * 0.25, hx, hz, 3.0),
                Sec::new(by + bh, hx, hz, 3.0),
            ];
            b.loft_y("yoke_hull", &secs, M::Body2, 36, [xc, 0.0, 0.0]);
        } else if !hull_off {
            b.rbox(
                "yoke_hull",
                [wbar, bh, hull_d],
                [xc, by + bh / 2.0, 0.0],
                M::Body2,
                0.3,
                24,
                14,
            );
        }
        b.cyl(
            "pan_neck",
            wbar.min(yd) * 0.3,
            0.006,
            [0.0, by + 0.003, 0.0],
            Axis::Y,
            M::Body,
            28,
        );
    } else {
        // A side-arm trunnion U-bail: arms up the head sides to the tilt axis.
        let mut t = if body.shape == BodyShape::Bar {
            0.014
        } else {
            cl((yw - hw) / 2.0 - gap, 0.012, 0.035)
        };
        t = t.min(wtot / 2.0 - hw / 2.0 - gap - 0.006).max(0.004);
        let sd = if m.kind == MountType::Yoke2 {
            cl(yd * 0.8, 0.02, 0.07)
        } else {
            cl(yd * 0.8, 0.012, 0.05)
        };
        if let Some(at) = arm_t {
            t = 0.004f64.max(at.min(wtot / 2.0 - hw / 2.0 - gap));
        }
        let xs = xs_of(t);
        for &x in &xs {
            let sgn = if x > 0.0 { 1.0 } else { -1.0 };
            match arm {
                Some(arm) => yoke_arm2(b, x, sgn, arm, by, y1, t, sd)?,
                None => yoke_arm(
                    b,
                    "bail_arm",
                    "bail_arm_top",
                    x,
                    t,
                    sd,
                    by,
                    y1,
                    &[1.0, 1.0, 0.95, 0.9, 0.85, 0.8],
                ),
            }
            b.cyl(
                "knob",
                0.02f64.min(hd * 0.12) + 0.004,
                0.006,
                [x + sgn * (t / 2.0 + 0.003), ty, 0.0],
                Axis::X,
                M::Metal,
                12,
            );
        }
        yoke_shape(b, yk, &xs, t, sd, by, ty, bottom);
        let (wbar, xc) = bar_span(t);
        b.rbox(
            "bail_bar",
            [wbar, t * 1.1, sd],
            [xc, by + 0.004 + t * 0.55, 0.0],
            M::Body2,
            0.4,
            12,
            6,
        );
    }
    Ok(())
}

/// The steering element inside a deflector's fixed housing; it owns the
/// pan/tilt joints.
fn deflector_element(b: &mut Rig, f: &Fixture, hw: f64, hc: f64, hl: f64) -> Res {
    let Some(d) = &f.deflector else {
        return fail("deflector template without a deflector");
    };
    let j = &f.rig.joints;
    let (at, z0) = match d.pos {
        // Fixture axes into the head frame: (x, -z, y).
        Some([x, y, z]) => (tr(x, -z, cl(y, -hl * 0.4, hl * 0.4)), 0.0),
        None => (DMat4::IDENTITY, hl / 2.0 - 0.02),
    };
    let pos = Some(DVec3::new(0.0, 0.0, z0));
    let element = |b: &mut Rig| match d.element {
        DeflectorElement::Drum => b.cyl(
            "drum",
            hw.min(hc) * 0.22,
            hw * 0.7,
            [0.0, 0.0, z0],
            Axis::X,
            M::Metal,
            24,
        ),
        DeflectorElement::Galvo => b.box_(
            "galvo_mirror",
            [hw.min(hc) * 0.12, hw.min(hc) * 0.12, 0.004],
            [0.0, 0.0, z0],
            M::Metal,
        ),
        DeflectorElement::Mirror => b.at(tr(0.0, 0.0, z0) * rx((-45f64).to_radians()), |b| {
            b.box_(
                "mirror",
                [hw * 0.6, hc.min(hl) * 0.45, 0.006],
                [0.0; 3],
                M::Metal,
            );
        }),
    };
    let tilt_then = |b: &mut Rig| match &j.tilt {
        Some(t) => b.joint(
            "tilt",
            Some(joint_rec(JointKind::Tilt, DVec3::X, t)),
            None,
            pos,
            element,
        ),
        None => element(b),
    };
    b.at(at, |b| match &j.pan {
        Some(p) => b.joint(
            "pan",
            Some(joint_rec(JointKind::Pan, DVec3::Y, p)),
            None,
            pos,
            tilt_then,
        ),
        None => tilt_then(b),
    });
    Ok(())
}

/// An array: `n` heads on a carrier, the carrier moved by `rig.joints`, each
/// head by `array.per_head`.
pub(crate) fn build_array(b: &mut Rig, f: &Fixture) -> Res {
    let Some(a) = &f.array else {
        return fail("array template without an array");
    };
    let n = int(a.n).max(0) as usize;
    let c = &a.carrier;
    let body = &f.rig.body;
    let j = &f.rig.joints;
    let (w, d, bh) = (c.w, c.d, c.h);
    let h = f.dims.size[1];
    let (hw, hd, hl) = (body.w, body.d, body.len);
    let yoked = a.element_mount.is_some();
    let y0 = if yoked { bh } else { bh + hl / 2.0 };
    let pos: Vec<(f64, f64, f64, f64)> = match c.kind {
        CarrierKind::Bar => {
            let pitch = w / n as f64;
            (0..n)
                .map(|i| ((i as f64 - (n as f64 - 1.0) / 2.0) * pitch, y0, 0.0, 0.0))
                .collect()
        }
        CarrierKind::Hub | CarrierKind::Ring => {
            let big = w.max(d) / 2.0;
            let rr = (big - hw / 2.0).max(hw * n as f64 / TAU * 0.9);
            (0..n)
                .map(|i| {
                    let a = TAU * i as f64 / n as f64;
                    (rr * a.sin(), y0, rr * a.cos(), a)
                })
                .collect()
        }
        CarrierKind::Tower => {
            let pitch = (h - bh) / n as f64;
            (0..n)
                .map(|i| (0.0, bh + (i as f64 + 0.5) * pitch, 0.0, 0.0))
                .collect()
        }
        CarrierKind::Grid => {
            let (Some(cols), Some(rows)) = (c.cols, c.rows) else {
                return fail("grid carrier without rows and cols");
            };
            let (cols, rows) = (int(cols), int(rows));
            (0..n as i64)
                .map(|i| {
                    let x = ((i % cols) as f64 - (cols as f64 - 1.0) / 2.0) * (w / cols as f64);
                    let z = ((i / cols) as f64 - (rows as f64 - 1.0) / 2.0) * (d / rows as f64);
                    (x, y0, z, 0.0)
                })
                .collect()
        }
    };
    let spin = j.spin.as_deref().unwrap_or(&[]);
    let beam_y = a.beam.as_deref() == Some("y");
    if !yoked && a.beam.is_none() {
        return fail("array without element_mount or beam");
    }
    let elements = |b: &mut Rig| -> Res {
        for (i, &(x, y, z, yaw)) in pos.iter().enumerate() {
            b.at(tr(x, y, z) * rot(yaw, DVec3::Y), |b| {
                b.joint(&format!("element_{i}"), None, Some(i), None, |b| {
                    let names = [
                        format!("element_{i}_pan"),
                        format!("element_{i}_tilt"),
                        format!("head_{i}"),
                    ];
                    if let Some(em) = &a.element_mount {
                        let chain = Chain {
                            size: f.dims.size,
                            template: Template::Luminaire,
                            mount: em,
                            body,
                            faces: &f.faces,
                            pan: a.per_head.pan.as_ref(),
                            tilt: a.per_head.tilt.as_ref(),
                            spin,
                            slide: &[],
                            fixture: None,
                            names,
                            element: Some(i),
                        };
                        return build_luminaire(b, &chain);
                    }
                    let head = |b: &mut Rig| -> Res {
                        let turn = if beam_y {
                            rx(-FRAC_PI_2)
                        } else {
                            DMat4::IDENTITY
                        };
                        b.at(turn, |b| {
                            b.joint(&names[2], None, Some(i), None, |b| {
                                let (hc, len) = if beam_y {
                                    (hd, hl)
                                } else {
                                    (((h - bh) / n as f64 * 0.9).max(0.02), hd)
                                };
                                build_head(b, body, &f.faces, spin, &[], hw, hc, len)
                            })
                        })
                    };
                    let tilt_then = |b: &mut Rig| match &a.per_head.tilt {
                        Some(t) => b.joint(
                            &names[1],
                            Some(joint_rec(JointKind::Tilt, DVec3::X, t)),
                            Some(i),
                            None,
                            head,
                        ),
                        None => head(b),
                    };
                    match &a.per_head.pan {
                        Some(p) => b.joint(
                            &names[0],
                            Some(joint_rec(JointKind::Pan, DVec3::Y, p)),
                            Some(i),
                            None,
                            tilt_then,
                        ),
                        None => tilt_then(b),
                    }
                })
            })?;
        }
        Ok(())
    };
    let carrier = |b: &mut Rig| -> Res {
        match c.kind {
            CarrierKind::Bar => {
                b.rbox(
                    "carrier",
                    [w + 0.02, bh * 0.7, (hd * 1.05).max(0.05)],
                    [0.0, bh * 0.45, 0.0],
                    M::Body2,
                    0.2,
                    24,
                    14,
                );
                b.rbox(
                    "carrier_rail",
                    [w - 0.02, bh * 0.3, (hd * 0.7).max(0.04)],
                    [0.0, bh * 0.85, 0.0],
                    M::Body,
                    0.25,
                    24,
                    14,
                );
                for sg in [-1.0, 1.0] {
                    b.rbox(
                        "end_bracket",
                        [0.02, bh * 1.25, (hd * 1.1).max(0.055)],
                        [sg * (w / 2.0 + 0.01), bh * 0.5, 0.0],
                        M::Body2,
                        0.2,
                        24,
                        14,
                    );
                    b.cyl(
                        "end_knob",
                        0.015f64.min(bh * 0.2),
                        0.012,
                        [sg * (w / 2.0 + 0.026), bh * 0.5, 0.0],
                        Axis::X,
                        M::Metal,
                        12,
                    );
                }
            }
            CarrierKind::Hub | CarrierKind::Ring => {
                let big = w.max(d) / 2.0;
                b.cyl(
                    "carrier",
                    big * 0.9,
                    bh,
                    [0.0, bh / 2.0, 0.0],
                    Axis::Y,
                    M::Body2,
                    36,
                );
            }
            CarrierKind::Tower => {
                let mr = cl(hw * 0.3, 0.004, 0.02);
                b.cyl(
                    "mast",
                    mr,
                    h,
                    [0.0, h / 2.0, -hd / 2.0 - mr],
                    Axis::Y,
                    M::Metal,
                    28,
                );
                b.cyl(
                    "foot",
                    cl(hw.max(hd) * 0.5, 0.01, 0.12),
                    0.02,
                    [0.0, 0.01, -hd * 0.25],
                    Axis::Y,
                    M::Body2,
                    28,
                );
            }
            CarrierKind::Grid => b.rbox(
                "carrier",
                [w, bh, d],
                [0.0, bh / 2.0, 0.0],
                M::Body2,
                0.25,
                24,
                14,
            ),
        }
        match &j.tilt {
            Some(t) => b.joint(
                "tilt",
                Some(joint_rec(JointKind::Tilt, DVec3::X, t)),
                None,
                Some(DVec3::new(0.0, bh + hl / 2.0, 0.0)),
                elements,
            ),
            None => elements(b),
        }
    };
    let whole = |b: &mut Rig| match &j.pan {
        Some(p) => b.joint(
            "pan",
            Some(joint_rec(JointKind::Pan, DVec3::Y, p)),
            None,
            Some(DVec3::ZERO),
            carrier,
        ),
        None => carrier(b),
    };
    if c.hang.unwrap_or(false) {
        b.at(tr(0.0, h, 0.0) * rot(PI, DVec3::Z), whole)
    } else {
        whole(b)
    }
}

/// The fixture's own chain (template `luminaire` or `deflector`).
pub(crate) fn fixture_chain(f: &Fixture) -> Chain<'_> {
    let j = &f.rig.joints;
    Chain {
        size: f.dims.size,
        template: f.template,
        mount: &f.rig.mount,
        body: &f.rig.body,
        faces: &f.faces,
        pan: j.pan.as_ref(),
        tilt: j.tilt.as_ref(),
        spin: j.spin.as_deref().unwrap_or(&[]),
        slide: j.slide.as_deref().unwrap_or(&[]),
        fixture: Some(f),
        names: ["pan".into(), "tilt".into(), "head".into()],
        element: None,
    }
}
