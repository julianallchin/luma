//! Faces: the light-emitting side of a head. A face is a recursive node tree
//! (`nodes`), panel zones (`emitter` zones) or pattern zones (round/lens heads).
//!
//! Cells are allocated here, in the order CONTROL.md §5 calls canonical: faces
//! in spec order, zones or nodes depth-first in each layout's slot order.

use std::f64::consts::PI;

use glam::DVec3;

use crate::mesh::{self, Mesh};
use crate::nodes;
use crate::rig::{fail, int, ry, Axis, Res, Rig, Size};
use crate::spec::{
    EmitterKind, Face, FaceSide, MaterialSlot as M, Zone, ZoneCells, ZoneLayout, ZoneRegion,
};

/// Where a face sits in its head frame: `fw x fh` at depth `z`, round or not.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Geom {
    pub fw: f64,
    pub fh: f64,
    pub z: f64,
    pub rnd: bool,
}

/// Python's `round(x, 3)`, close enough for a sort key.
fn round3(x: f64) -> f64 {
    (x * 1000.0).round_ties_even() / 1000.0
}

/// Emitter centres of a pattern zone, fitted into `fw x fh` (an ellipse when
/// `rnd`), and the emitter size. Returns exactly `layout.n` points (fewer only
/// when a ring list is short).
pub(crate) fn layout_pts(lay: &ZoneLayout, fw: f64, fh: f64, rnd: bool) -> (Vec<[f64; 2]>, f64) {
    let kind = lay.kind.as_deref().unwrap_or("single");
    let n = int(lay.n.filter(|&v| v != 0.0).unwrap_or(1.0)).max(1) as usize;
    let stag = lay.stagger.unwrap_or(false);
    if kind == "points" {
        if let Some(points) = lay.points.as_ref().filter(|p| !p.is_empty()) {
            let d = 0.18 * fw.min(fh);
            let pts = points
                .iter()
                .map(|p| [p[0] * (fw / 2.0 * 0.9), p[1] * (fh / 2.0 * 0.9)])
                .collect();
            return (pts, d);
        }
    }
    let one = kind == "single" || n == 1;
    let mut pts: Vec<[f64; 2]> = if one {
        vec![[0.0, 0.0]]
    } else if kind == "line" {
        let rows = int(lay.rows.filter(|&v| v != 0.0).unwrap_or(1.0)).max(1) as usize;
        let cols = n.div_ceil(rows);
        let mut p: Vec<[f64; 2]> = (0..rows)
            .flat_map(|r| {
                (0..cols).map(move |c| {
                    [
                        c as f64 + if stag && r % 2 == 1 { 0.5 } else { 0.0 },
                        r as f64,
                    ]
                })
            })
            .take(n)
            .collect();
        centre(&mut p);
        if fh > fw {
            for q in &mut p {
                q.swap(0, 1);
            }
        }
        p
    } else if kind == "rings" {
        let rg: Vec<i64> = match lay.rings.as_ref().filter(|r| !r.is_empty()) {
            Some(r) => r.iter().map(|&v| int(v)).collect(),
            None => vec![n as i64],
        };
        let mut pl = Vec::new();
        for (i, &m) in rg.iter().enumerate() {
            if m == 1 && i == 0 {
                pl.push([0.0, 0.0]);
                continue;
            }
            let r = if rg[0] == 1 { i as f64 } else { i as f64 + 1.0 };
            let a0 = i as f64 * 0.35;
            for t in 0..m.max(0) {
                let a = a0 + 2.0 * PI * t as f64 / m as f64;
                pl.push([r * a.cos(), r * a.sin()]);
            }
        }
        pl.truncate(n);
        if pl.is_empty() {
            pl.push([0.0, 0.0]);
        }
        pl
    } else if kind == "hex" {
        let k = (n as f64).sqrt() as i64 + 3;
        let mut pl: Vec<[f64; 2]> = Vec::new();
        for i in -k..=k {
            for j in -k..=k {
                pl.push([i as f64 + j as f64 / 2.0, j as f64 * 3f64.sqrt() / 2.0]);
            }
        }
        pl.sort_by(|a, b| {
            let ka = (round3(a[0].hypot(a[1])), a[1].atan2(a[0]));
            let kb = (round3(b[0].hypot(b[1])), b[1].atan2(b[0]));
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        });
        pl.truncate(n);
        pl
    } else {
        grid(lay, n, fw, fh, stag)
    };
    let d = 1.0;
    let ex = pts.iter().map(|p| p[0].abs()).fold(f64::MIN, f64::max) + d / 2.0;
    let ey = pts.iter().map(|p| p[1].abs()).fold(f64::MIN, f64::max) + d / 2.0;
    let s = if rnd && !one {
        0.5 * fw.min(fh) * 0.94
            / (pts
                .iter()
                .map(|p| p[0].hypot(p[1]))
                .fold(f64::MIN, f64::max)
                + d / 2.0)
    } else if one {
        0.5 * fw.min(fh) * 0.94
    } else {
        (fw * 0.5 * 0.96 / ex).min(fh * 0.5 * 0.96 / ey)
    };
    for p in &mut pts {
        p[0] *= s;
        p[1] *= s;
    }
    (pts, d * s * if one { 0.7 } else { 0.82 })
}

fn centre(p: &mut [[f64; 2]]) {
    for axis in 0..2 {
        let lo = p.iter().map(|q| q[axis]).fold(f64::MAX, f64::min);
        let hi = p.iter().map(|q| q[axis]).fold(f64::MIN, f64::max);
        let mid = (hi + lo) / 2.0;
        for q in p.iter_mut() {
            q[axis] -= mid;
        }
    }
}

fn grid(lay: &ZoneLayout, n: usize, fw: f64, fh: f64, stag: bool) -> Vec<[f64; 2]> {
    let pos = |v: Option<f64>| v.map(int).filter(|&v| v != 0);
    let (mut rows, mut cols) = (pos(lay.rows), pos(lay.cols));
    if let (Some(r), Some(c)) = (rows, cols) {
        if r * c < n as i64 {
            rows = None;
            cols = None;
        }
    }
    if let (Some(r), Some(c)) = (rows, cols) {
        if r != c && ((c > r) != (fw > fh)) && (fw / fh).max(fh / fw) > 1.3 {
            rows = Some(c);
            cols = Some(r);
        }
    }
    let (rows, cols) = match (rows, cols) {
        (Some(r), Some(c)) => (r, c),
        _ => {
            let ar = fw / fh.max(1e-6);
            let mut rows = ((n as f64 / ar.max(0.05)).sqrt().round_ties_even() as i64).max(1);
            let cols = (n as f64 / rows as f64).ceil() as i64;
            if rows * cols < n as i64 {
                rows += 1;
            }
            (rows, cols)
        }
    };
    let seams = lay.seams.as_ref();
    let ec = seams.and_then(|s| s.every_c).map_or(0, int);
    let er = seams.and_then(|s| s.every_r).map_or(0, int);
    let gap = seams.and_then(|s| s.gap).unwrap_or(0.35);
    let mut pts = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            let x = c as f64
                + if stag && r % 2 == 1 { 0.5 } else { 0.0 }
                + if ec != 0 { gap * (c / ec) as f64 } else { 0.0 };
            let y = -(r as f64 + if er != 0 { gap * (r / er) as f64 } else { 0.0 });
            pts.push([x, y]);
        }
    }
    centre(&mut pts);
    if pts.len() > n {
        // Keep the n nearest the centre, in their original order. The
        // reference builder's argsort breaks distance ties in no defined
        // order; this keeps the earlier of two equal ones.
        let mut order: Vec<usize> = (0..pts.len()).collect();
        order.sort_by(|&a, &b| {
            let (da, db) = (pts[a][0].hypot(pts[a][1]), pts[b][0].hypot(pts[b][1]));
            db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut keep: Vec<usize> = order[pts.len() - n..].to_vec();
        keep.sort_unstable();
        pts = keep.into_iter().map(|i| pts[i]).collect();
    }
    pts
}

/// Draw one face at depth `z` of the current head frame.
pub(crate) fn build_face(b: &mut Rig, face: &Face, geom: Geom) -> Res {
    let turn = if face.on == Some(FaceSide::Back) {
        ry(PI)
    } else {
        glam::DMat4::IDENTITY
    };
    b.at(turn, |b| {
        if let Some(root) = &face.root {
            return nodes::build_face(b, root, geom);
        }
        let Some(zones) = &face.zones else {
            return fail("face has neither root nor zones");
        };
        if zones.iter().any(|z| z.emitter.is_some()) {
            build_zones(b, zones, geom)
        } else {
            pattern_zones(b, zones, geom)
        }
    })
}

pub(crate) fn faces_on(b: &mut Rig, faces: &[Face], geom: Geom) -> Res {
    for f in faces {
        build_face(b, f, geom)?;
    }
    Ok(())
}

fn layout_n(z: &Zone) -> i64 {
    int(z
        .layout
        .as_ref()
        .and_then(|l| l.n)
        .filter(|&v| v != 0.0)
        .unwrap_or(1.0))
}

/// Pattern zones: lenses and pixels laid out over a round or square face.
fn pattern_zones(b: &mut Rig, zones: &[Zone], g: Geom) -> Res {
    b.colour = None;
    let Geom { fw, fh, z, rnd } = g;
    for zone in zones {
        let Some(lay) = &zone.layout else {
            return fail("pattern zone without a layout");
        };
        let centre_region = match &zone.region {
            Some(ZoneRegion::Centre { kind, frac }) => Some((kind.as_deref(), frac.unwrap_or(0.4))),
            _ => None,
        };
        let (mut zw, mut zh) = (fw, fh);
        let lens_d = zone.lens_d.filter(|&v| v != 0.0);
        if let Some(ld) = lens_d {
            if layout_n(zone) == 1 {
                zw = ld.min(fw).min(fh);
                zh = zw;
            }
        }
        if let Some((Some("centre"), frac)) = centre_region {
            zw = fw * frac;
            zh = fh * frac;
        }
        let optic = zone
            .optic
            .as_deref()
            .filter(|o| !o.is_empty())
            .unwrap_or("lens");
        let n = layout_n(zone);
        let pm = if optic == "diffuser" {
            M::Diffuser
        } else if optic == "open" || optic == "none" || (1 < n && n <= 61) {
            M::Body2
        } else {
            M::Lens
        };
        // A region dict with any key raises the face a little.
        let has_region = matches!(&zone.region, Some(ZoneRegion::Centre { kind, frac }) if kind.is_some() || frac.is_some())
            || matches!(&zone.region, Some(ZoneRegion::Rect(_)));
        let mut zz = z + if has_region { 0.004 } else { 0.0 };
        if rnd {
            b.cyl(
                "face_plate",
                zw.min(zh) / 2.0,
                0.006,
                [0.0, 0.0, zz - 0.002],
                Axis::Z,
                pm,
                36,
            );
        } else {
            b.box_("face_plate", [zw, zh, 0.006], [0.0, 0.0, zz - 0.002], pm);
        }
        let (pts, mut cw) = layout_pts(lay, zw * 0.94, zh * 0.94, rnd);
        let mut ch = cw;
        let shape = zone
            .cell
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("circle");
        if let Some(ld) = lens_d {
            if pts.len() > 1 {
                cw = ld;
                ch = ld;
            }
        }
        let nem = pts.len();
        if nem == 1 && matches!(optic, "lens" | "fresnel" | "flat") {
            let r = zw.min(zh) / 2.0;
            if !rnd {
                b.cyl(
                    "lens_bezel",
                    r * 0.95,
                    0.008,
                    [0.0, 0.0, zz + 0.001],
                    Axis::Z,
                    M::Body2,
                    36,
                );
            }
            b.cyl(
                "lens",
                r * 0.9,
                0.012,
                [0.0, 0.0, zz + 0.003],
                Axis::Z,
                M::Lens,
                36,
            );
            if optic == "fresnel" {
                for f in [0.8, 0.6, 0.4] {
                    b.cyl(
                        "fresnel_ring",
                        r * f,
                        0.004,
                        [0.0, 0.0, zz + 0.010],
                        Axis::Z,
                        M::Body2,
                        36,
                    );
                }
            }
            cw = zw.min(zh) * 0.5;
            ch = cw;
        }
        if 1 < nem && nem <= 61 && shape == "circle" && optic != "open" && cw > 0.008 {
            for p in &pts {
                b.cyl(
                    "cell_bezel",
                    cw * 0.56,
                    0.008,
                    [p[0], p[1], zz + 0.004],
                    Axis::Z,
                    M::Body2,
                    14,
                );
                b.dome(
                    "cell_lens",
                    [p[0], p[1], zz + 0.008],
                    cw * 0.47,
                    cw * 0.3,
                    M::Diffuser,
                );
            }
            zz += 0.008 + cw * 0.26;
            cw *= 0.55;
        } else if nem > 1 && optic == "open" && rnd && shape == "circle" {
            for p in &pts {
                b.cyl(
                    "pixel_pad",
                    cw * 0.62,
                    0.002,
                    [p[0], p[1], zz + 0.001],
                    Axis::Z,
                    M::Body2,
                    12,
                );
            }
            zz += 0.0015;
        }
        let line1 = lay.kind.as_deref() == Some("line")
            && lay.rows.filter(|&v| v != 0.0).unwrap_or(1.0) == 1.0;
        if line1 && shape == "square" {
            ch = (if zw >= zh { zh } else { zw } * 0.8).min(ch * 5.0);
        }
        let cells = match zone.cells {
            Some(ZoneCells::Count(c)) if c != 0.0 => int(c),
            _ => nem as i64,
        };
        let nc = cells.min(nem as i64).max(1) as usize;
        let base = b.next_cell;
        b.next_cell += nc as u32;
        for (j, p) in pts.iter().enumerate() {
            let k = base + ((j * nc) / nem) as u32;
            let size = if shape == "square" {
                Size::Wh(cw, ch)
            } else {
                Size::D(cw)
            };
            b.emit_cell(k, [p[0], p[1], zz + 0.002], shape, size, 0.004, M::Emitter);
        }
        b.source(
            DVec3::new(0.0, 0.0, zz + 0.006),
            (base..base + nc as u32).collect(),
        );
    }
    Ok(())
}

/// Most emitter meshes per fixture; dense zones coarsen their pitch to fit.
const EMIT_BUDGET: f64 = 1500.0;

fn spread(total: i64, c: i64) -> Vec<i64> {
    (0..c)
        .map(|j| (j + 1) * total / c - j * total / c)
        .collect()
}

struct Resolved {
    x0: f64,
    yt: f64,
    cw: f64,
    ch: f64,
    cr: usize,
    cc: usize,
    counts: Vec<i64>,
    cwid: Option<Vec<f64>>,
    /// Emitter rows/cols per cell when the pitch decides them.
    pitch_rc: Option<(f64, f64)>,
}

fn resolve_zone(zone: &Zone, fw: f64, fh: f64, scale: f64) -> Res<Resolved> {
    let Some(&ZoneRegion::Rect([u0, v0, u1, v1])) = zone.region.as_ref() else {
        return fail("panel zone without a region");
    };
    let Some(ZoneCells::Grid { rows, cols }) = zone.cells else {
        return fail("panel zone without cells {rows, cols}");
    };
    let e = zone.emitter.as_ref().expect("a panel zone has an emitter");
    let (x0, x1, yt, yb) = match zone.plane.as_deref().filter(|p| !p.is_empty()) {
        Some(pl) => {
            let (mut x0, mut x1, mut yt, mut yb) = (
                (u0 - 0.5) * pl[0],
                (u1 - 0.5) * pl[0],
                (0.5 - v0) * pl[1],
                (0.5 - v1) * pl[1],
            );
            let lim = (fw / 2.0, fh / 2.0);
            x0 = x0.max(-lim.0);
            x1 = x1.min(lim.0);
            yb = yb.max(-lim.1);
            yt = yt.min(lim.1);
            (x0, x1, yt, yb)
        }
        None => (
            (u0 - 0.5) * fw,
            (u1 - 0.5) * fw,
            (0.5 - v0) * fh,
            (0.5 - v1) * fh,
        ),
    };
    let (cr, cc) = (int(rows), int(cols));
    let (cw, ch) = ((x1 - x0) / cc as f64, (yt - yb) / cr as f64);
    let kind = e.kind.unwrap_or(EmitterKind::Led);
    let runs = zone.runs.as_ref().filter(|r| !r.is_empty());
    let cwid = runs.map(|r| {
        let sum: f64 = r.iter().sum();
        r.iter().map(|q| (x1 - x0) * q / sum).collect()
    });
    let mut pitch_rc = None;
    let cells = (cr * cc).max(0) as usize;
    let counts = if let Some(p) = zone.points.as_ref().filter(|p| !p.is_empty()) {
        vec![1; p.len()]
    } else if matches!(kind, EmitterKind::Tube | EmitterKind::Strip) {
        vec![1; cells]
    } else if let Some(r) = runs {
        r.iter().map(|&v| int(v)).collect()
    } else if let Some(total) = e.total.filter(|&t| t != 0.0) {
        spread(int(total), cr * cc)
    } else {
        let (mut er, mut ec) = (e.rows, e.cols);
        if let Some(p) = e.pitch_mm.filter(|&p| p != 0.0) {
            let p = p * scale / 1000.0;
            let r = (int((ch - 0.002) / p)).max(1) as f64;
            let c = (int((cw - 0.002) / p)).max(1) as f64;
            pitch_rc = Some((r, c));
            er = Some(r);
            ec = Some(c);
        }
        let per = |v: Option<f64>| int(v.filter(|&x| x != 0.0).unwrap_or(1.0)).max(1);
        vec![per(er) * per(ec); cells]
    };
    Ok(Resolved {
        x0,
        yt,
        cw,
        ch,
        cr: cr as usize,
        cc: cc as usize,
        counts,
        cwid,
        pitch_rc,
    })
}

/// Panel zones: ribs, per-cell emitter grids, tubes and white-only strips.
fn build_zones(b: &mut Rig, zones: &[Zone], g: Geom) -> Res {
    let Geom { fw, fh, z, rnd } = g;
    let mut scale = 1.0;
    for _ in 0..4 {
        let mut tot = 0;
        for zz in zones {
            tot += resolve_zone(zz, fw, fh, scale)?.counts.iter().sum::<i64>();
        }
        if tot as f64 <= EMIT_BUDGET {
            break;
        }
        scale *= (tot as f64 / EMIT_BUDGET).sqrt() * 1.02;
    }
    let n0 = b.items.len();
    let tf = b.t;
    for zz in zones {
        if let Some([u0, v0, u1, v1]) = zz.window {
            b.box_(
                "window",
                [(u1 - u0) * fw, (v1 - v0) * fh, 0.003],
                [
                    ((u0 + u1) / 2.0 - 0.5) * fw,
                    (0.5 - (v0 + v1) / 2.0) * fh,
                    z + 0.001,
                ],
                M::Diffuser,
            );
        }
    }
    for zone in zones {
        let g = resolve_zone(zone, fw, fh, scale)?;
        let e = zone.emitter.as_ref().expect("checked by the caller");
        let kind = e.kind.unwrap_or(EmitterKind::Led);
        let (cw, ch, cr, cc) = (g.cw, g.ch, g.cr, g.cc);
        let (rw, rh) = (cw * cc as f64, ch * cr as f64);
        let (cxm, cym) = (g.x0 + rw / 2.0, g.yt - rh / 2.0);
        let rib = 0.0015f64.min(0.12 * rw.min(rh));
        if rnd {
            if zone.window.is_none() {
                b.ellipse_plate(
                    "zone_plate",
                    rw.min(fw) / 2.0,
                    rh.min(fh) / 2.0,
                    0.0035,
                    [cxm, cym, z + 0.0005],
                    M::Body,
                );
            }
        } else {
            if zone.window.is_none() {
                b.box_(
                    "zone_plate",
                    [rw, rh, 0.0035],
                    [cxm, cym, z + 0.0005],
                    M::Body,
                );
            }
            for sy in [1.0, -1.0] {
                b.box_(
                    "rib",
                    [rw + 2.0 * rib, rib, 0.004],
                    [cxm, cym + sy * (rh / 2.0 + rib / 2.0), z + 0.0008],
                    M::Body2,
                );
            }
            for sx in [-1.0, 1.0] {
                b.box_(
                    "rib",
                    [rib, rh, 0.004],
                    [cxm + sx * (rw / 2.0 + rib / 2.0), cym, z + 0.0008],
                    M::Body2,
                );
            }
        }
        let white = e.colour.as_deref() == Some("white");
        b.colour.clone_from(&e.colour);
        if let Some(points) = zone.points.as_ref().filter(|p| !p.is_empty()) {
            let cid: Vec<u32> = match zone.cell_ids.as_ref().filter(|c| !c.is_empty()) {
                Some(c) => c.iter().map(|&v| int(v) as u32).collect(),
                None => (b.next_cell..b.next_cell + points.len() as u32).collect(),
            };
            b.next_cell = b.next_cell.max(cid.iter().max().copied().unwrap_or(0) + 1);
            let em = if white { M::EmitterW } else { M::Emitter };
            let circle = e.shape.as_deref() == Some("circle");
            for (i, p) in points.iter().enumerate() {
                let (px, py, pw, ph) = match (p.as_slice(), zone.point_size) {
                    ([x, y, w, h, ..], _) => (*x, *y, *w, *h),
                    ([x, y], Some([w, h])) => (*x, *y, w, h),
                    _ => return fail("zone point without a size"),
                };
                let Some(&k) = cid.get(i) else {
                    return fail("zone cell_ids shorter than its points");
                };
                let pzz = z + 0.0024;
                if circle && points.len() <= 200 {
                    b.cyl(
                        "lens_bezel",
                        pw.min(ph) * 0.55,
                        0.003,
                        [px, py, pzz - 0.0004],
                        Axis::Z,
                        M::Body2,
                        16,
                    );
                }
                if circle {
                    b.emit_cell(
                        k,
                        [px, py, pzz + 0.0015],
                        "circle",
                        Size::D(pw.min(ph) * 0.98),
                        0.0035,
                        em,
                    );
                } else {
                    b.emit_cell(
                        k,
                        [px, py, pzz],
                        "square",
                        Size::Wh(pw * 0.96, ph * 0.96),
                        0.003,
                        em,
                    );
                }
            }
            let mut cells = cid.clone();
            cells.sort_unstable();
            cells.dedup();
            b.source(DVec3::new(0.0, 0.0, z + 0.0084), cells);
            continue;
        }
        let cid: Vec<u32> = match zone.cell_ids.as_ref().filter(|c| !c.is_empty()) {
            Some(c) => c.iter().map(|&v| int(v) as u32).collect(),
            None => (b.next_cell..b.next_cell + (cr * cc) as u32).collect(),
        };
        b.next_cell = b.next_cell.max(cid.iter().max().copied().unwrap_or(0) + 1);
        let mut em = if white { M::EmitterW } else { M::Emitter };
        let zz = z + 0.0024;
        let gap = 0.0008f64.min(0.05 * cw.min(ch));
        let run_white = zone.run_white.as_ref().filter(|r| !r.is_empty());
        let (e_rows, e_cols) = g
            .pitch_rc
            .map_or((e.rows, e.cols), |(r, c)| (Some(r), Some(c)));
        for ri in 0..cr {
            for ci in 0..cc {
                let Some(&k) = cid.get(ri * cc + ci) else {
                    return fail("zone cell_ids shorter than its cells");
                };
                if let Some(rwh) = run_white {
                    em = if rwh.get(ci).copied().unwrap_or(false) {
                        M::EmitterW
                    } else {
                        M::Emitter
                    };
                }
                let (cw_i, ccx) = match &g.cwid {
                    Some(cwid) => (
                        cwid[ci],
                        g.x0 + cwid[..ci].iter().sum::<f64>() + cwid[ci] / 2.0,
                    ),
                    None => (cw, g.x0 + (ci as f64 + 0.5) * cw),
                };
                let ccy = g.yt - (ri as f64 + 0.5) * ch;
                let (iw, ih) = (cw_i - 2.0 * gap, ch - 2.0 * gap);
                let n = *g.counts.get(ri * cc + ci).ok_or_else(|| {
                    crate::rig::BuildError("zone runs shorter than its cells".into())
                })?;
                if kind == EmitterKind::Tube {
                    let r = (rh / 2.0 * 0.8).min(rw / 2.0);
                    let mut m: Mesh = mesh::cylinder(r.max(1e-4), (rw * 0.98).max(1e-3), 16);
                    m.transform(&ry(PI / 2.0));
                    b.emit_mesh(k, m, [ccx, ccy, zz + r * 0.6], em, [rw * 0.98, 2.0 * r]);
                    continue;
                }
                if kind == EmitterKind::Strip {
                    b.emit_cell(
                        k,
                        [ccx, ccy, zz],
                        "square",
                        Size::Wh((iw * 0.96).max(1e-4), (ih * 0.9).max(1e-4)),
                        0.003,
                        em,
                    );
                    continue;
                }
                if zone.cell_shape.as_deref() == Some("circle") {
                    let dd = iw.min(ih) * 0.94;
                    b.cyl(
                        "cell_bezel",
                        dd / 2.0 * 1.05,
                        0.004,
                        [ccx, ccy, zz - 0.0006],
                        Axis::Z,
                        M::Body2,
                        32,
                    );
                    if n <= 1 {
                        b.emit_cell(
                            k,
                            [ccx, ccy, zz + 0.0015],
                            "circle",
                            Size::D(dd),
                            0.0035,
                            em,
                        );
                        continue;
                    }
                    let p = dd * (PI / (2.0 * 3f64.sqrt() * n as f64)).sqrt() * 0.98;
                    let rr = dd / 2.0 - p * 0.5;
                    let rows = int(rr / (p * 0.866));
                    let cols = int(rr / p);
                    for row in (-rows - 1)..(rows + 2) {
                        for col in (-cols - 2)..(cols + 3) {
                            let x = (col as f64 + row.rem_euclid(2) as f64 * 0.5) * p;
                            let y = row as f64 * p * 0.866;
                            if x * x + y * y <= rr * rr {
                                b.emit_cell(
                                    k,
                                    [ccx + x, ccy + y, zz + 0.0015],
                                    "circle",
                                    Size::D(p * 0.88),
                                    0.003,
                                    em,
                                );
                            }
                        }
                    }
                    continue;
                }
                let mut er = int(e_rows.filter(|&v| v != 0.0).unwrap_or(1.0)).max(1);
                let mut ec = int(e_cols.filter(|&v| v != 0.0).unwrap_or(1.0)).max(1);
                if g.cwid.is_some() {
                    er = 1;
                    ec = n;
                } else if e.total.is_some_and(|t| t != 0.0) {
                    er = er.min(n).max(1);
                    ec = (n as f64 / er as f64).ceil() as i64;
                }
                let (px, py) = (iw / ec as f64, ih / er as f64);
                for j in 0..n {
                    let (rr, c) = (j / ec, j % ec);
                    let inrow = ec.min(n - rr * ec);
                    let ex = ccx - iw / 2.0 + (c as f64 + 0.5 + (ec - inrow) as f64 / 2.0) * px;
                    let ey = ccy + ih / 2.0 - (rr as f64 + 0.5) * py;
                    if kind == EmitterKind::Lens {
                        let d = e
                            .d_mm
                            .filter(|&v| v != 0.0)
                            .map_or(0.99 * px.min(py), |d| d / 1000.0);
                        if (cr * cc) as i64 * n <= 200 {
                            b.cyl(
                                "lens_bezel",
                                d * 0.58,
                                0.003,
                                [ex, ey, zz - 0.0004],
                                Axis::Z,
                                M::Body2,
                                16,
                            );
                        }
                        b.emit_cell(k, [ex, ey, zz + 0.0015], "circle", Size::D(d), 0.0035, em);
                    } else if e.shape.as_deref() == Some("circle") {
                        let d = e
                            .d_mm
                            .filter(|&v| v != 0.0)
                            .map_or(0.94 * px.min(py), |d| d / 1000.0);
                        b.emit_cell(k, [ex, ey, zz], "circle", Size::D(d), 0.003, em);
                    } else {
                        b.emit_cell(
                            k,
                            [ex, ey, zz],
                            "square",
                            Size::Wh(0.94 * px, 0.94 * py),
                            0.003,
                            em,
                        );
                    }
                }
            }
        }
        let mut cells = cid.clone();
        cells.sort_unstable();
        cells.dedup();
        b.source(DVec3::new(0.0, 0.0, zz + 0.006), cells);
    }
    if rnd {
        // A round face drops emitters, bezels and ribs whose outline leaves the
        // ellipse, every vertex of them, so long ribs do not cross the rim.
        let face_from_world = tf.inverse();
        let mut keep = Vec::with_capacity(b.items.len());
        for (i, it) in std::mem::take(&mut b.items).into_iter().enumerate() {
            let clipped = matches!(it.name, "cell_" | "lens_bezel" | "cell_bezel" | "rib");
            if i < n0 || !clipped {
                keep.push(it);
                continue;
            }
            let m = face_from_world * b.nodes[it.node].world;
            let inside = it.mesh.v.iter().all(|&v| {
                let p = m.transform_point3(v);
                (p.x / (fw / 2.0)).powi(2) + (p.y / (fh / 2.0)).powi(2) <= 1.02
            });
            if inside {
                keep.push(it);
            }
        }
        b.items = keep;
    }
    b.colour = None;
    Ok(())
}
