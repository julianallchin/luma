//! Recursive face layout (`faces[].root`): a node lays out child nodes or is
//! an emitter leaf. FORMAT.md, "Face nodes".

use std::collections::HashMap;
use std::f64::consts::PI;

use glam::DVec3;

use crate::face::Geom;
use crate::mesh;
use crate::rig::{fail, int, rx, ry, tr, Axis, Res, Rig, Size};
use crate::spec::{
    CellFlag, Diffuser, Dir, EmitterKind, LayoutKind, MaterialSlot as M, Node, NodeEmitter,
    NodeOutline,
};
use crate::{EmitterShape, LedPattern};

/// A child slot: centre and size, relative to the node centre.
type Slot = (f64, f64, f64, f64);

/// A die colour's material; anything else is a mixing emitter.
fn colour_material(c: &str) -> M {
    match c {
        "white" | "ww_cw" => M::EmitterW,
        "red" => M::EmitterR,
        "green" | "lime" | "mint" => M::EmitterG,
        "blue" | "cyan" => M::EmitterB,
        "amber" => M::EmitterA,
        "uv" => M::EmitterUv,
        _ => M::Emitter,
    }
}

fn inside(outline: Option<NodeOutline>, x: f64, y: f64, w: f64, h: f64) -> bool {
    match outline {
        Some(NodeOutline::Circle) => {
            (x / (w / 2.0)).powi(2) + (y / (h / 2.0)).powi(2) <= 1.0 + 1e-6
        }
        Some(NodeOutline::Hex) => {
            x.abs() <= w / 2.0
                && y.abs() <= h / 2.0
                && x.abs() / (w / 2.0) * 0.5 + y.abs() / (h / 2.0) <= 1.0
        }
        _ => x.abs() <= w / 2.0 + 1e-9 && y.abs() <= h / 2.0 + 1e-9,
    }
}

/// The child slots of `node` laid out in `w x h`, in canonical slot order.
fn slots(node: &Node, w: f64, h: f64) -> Vec<Slot> {
    let Some(l) = &node.layout else {
        return vec![(0.0, 0.0, w, h)];
    };
    let gap = l.gap.unwrap_or(0.0);
    match l.kind {
        LayoutKind::Single => vec![(0.0, 0.0, w, h)],
        LayoutKind::Line => {
            let n = int(l.n.unwrap_or(1.0)).max(1);
            let dir = l.dir.unwrap_or(if w >= h { Dir::X } else { Dir::Y });
            if dir == Dir::X {
                let p = w / n as f64;
                (0..n)
                    .map(|i| (-w / 2.0 + (i as f64 + 0.5) * p, 0.0, p * (1.0 - gap), h))
                    .collect()
            } else {
                let p = h / n as f64;
                (0..n)
                    .map(|i| (0.0, h / 2.0 - (i as f64 + 0.5) * p, w, p * (1.0 - gap)))
                    .collect()
            }
        }
        LayoutKind::Grid => {
            let r = int(l.rows.unwrap_or(1.0)).max(1);
            let c = int(l.cols.unwrap_or(1.0)).max(1);
            let st = l.stagger.unwrap_or(false);
            let px = w / (c as f64 + if st { 0.5 } else { 0.0 });
            let py = h / r as f64;
            let mut out = Vec::with_capacity((r * c) as usize);
            for i in 0..r {
                let off = if st && i % 2 == 1 { 0.5 * px } else { 0.0 };
                for j in 0..c {
                    out.push((
                        -w / 2.0 + off + (j as f64 + 0.5) * px,
                        h / 2.0 - (i as f64 + 0.5) * py,
                        px * (1.0 - gap),
                        py * (1.0 - gap),
                    ));
                }
            }
            out
        }
        LayoutKind::Rings => {
            let counts: Vec<i64> = l
                .counts
                .as_ref()
                .map_or(vec![1], |c| c.iter().map(|&v| int(v)).collect());
            let nr = counts.len();
            let ctr = counts.first() == Some(&1);
            let steps = nr - usize::from(ctr);
            let radii: Vec<f64> = match l.radii.as_ref().filter(|r| !r.is_empty()) {
                Some(r) => r.clone(),
                None => {
                    let mut r = if ctr { vec![0.0] } else { Vec::new() };
                    let (a, b) = if ctr { (1.0, 0.0) } else { (0.5, 0.5) };
                    for i in 0..steps {
                        r.push(
                            (i as f64 + a) / (steps as f64 + b) * (1.0 - 0.5 / nr.max(1) as f64),
                        );
                    }
                    r
                }
            };
            let pitch = w.min(h) / 2.0 / (nr as f64 - if ctr { 0.5 } else { 0.0 }).max(1.0);
            let phase = l.phase.unwrap_or(90.0).to_radians();
            let mut out = Vec::new();
            for (&n, &rr) in counts.iter().zip(&radii) {
                for j in 0..n.max(0) {
                    let a = 2.0 * PI * j as f64 / n.max(1) as f64 + phase;
                    let (x, y) = if n > 1 {
                        (rr * w / 2.0 * a.cos(), rr * h / 2.0 * a.sin())
                    } else {
                        (0.0, 0.0)
                    };
                    out.push((x, y, pitch, pitch));
                }
            }
            out
        }
        LayoutKind::Hex => {
            let n = int(l.n.unwrap_or(1.0)).max(1) as usize;
            let ol = node.outline;
            let area = w
                * h
                * if ol == Some(NodeOutline::Circle) {
                    PI / 4.0
                } else {
                    1.0
                };
            let mut p = (area / n as f64 * 2.0 / 3f64.sqrt()).sqrt() * 0.98;
            let mut pts = Vec::new();
            for _ in 0..12 {
                pts.clear();
                let rows = int(h / (p * 0.866)) + 2;
                let cols = int(w / p) + 2;
                for i in -rows..=rows {
                    for j in -cols..=cols {
                        let x = (j as f64 + i.rem_euclid(2) as f64 * 0.5) * p;
                        let y = i as f64 * p * 0.866;
                        if inside(ol, x, y, w - p, h - p) {
                            pts.push((x, y));
                        }
                    }
                }
                if pts.len() >= n {
                    break;
                }
                p *= 0.95;
            }
            pts.sort_by(|a, b| {
                let ka = (a.0 * a.0 + a.1 * a.1, a.1.atan2(a.0));
                let kb = (b.0 * b.0 + b.1 * b.1, b.1.atan2(b.0));
                ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
            });
            pts.into_iter().take(n).map(|(x, y)| (x, y, p, p)).collect()
        }
        LayoutKind::Stack => {
            let kids = node.children.as_ref().map_or(0, Vec::len);
            let dir = l.dir.unwrap_or(Dir::Y);
            let wt: Vec<f64> = l
                .weights
                .clone()
                .filter(|w| !w.is_empty())
                .unwrap_or_else(|| vec![1.0; kids]);
            let sum: f64 = wt.iter().sum();
            let tot = if sum == 0.0 { 1.0 } else { sum };
            let len = if dir == Dir::Y { h } else { w };
            let mut acc = 0.0;
            wt.iter()
                .map(|q| {
                    let s = len * q / tot;
                    let c = -len / 2.0 + acc + s / 2.0;
                    acc += s;
                    if dir == Dir::Y {
                        (0.0, -c, w, s * (1.0 - gap))
                    } else {
                        (c, 0.0, s * (1.0 - gap), h)
                    }
                })
                .collect()
        }
        LayoutKind::Place => l
            .boxes
            .iter()
            .flatten()
            .map(|&[u0, v0, u1, v1]| {
                (
                    (u0 + u1) / 2.0 * w - w / 2.0,
                    h / 2.0 - (v0 + v1) / 2.0 * h,
                    (u1 - u0) * w,
                    (v1 - v0) * h,
                )
            })
            .collect(),
        LayoutKind::Points => {
            let sz = l.size.unwrap_or(0.1);
            l.pts
                .iter()
                .flatten()
                .map(|&[px, py]| (px * w / 2.0, py * h / 2.0, sz * w, sz * h))
                .collect()
        }
    }
}

struct Ctx {
    z: f64,
    c0: u32,
    each: bool,
    colour_i: HashMap<Vec<usize>, usize>,
}

fn emitter_set(e: &NodeEmitter) -> bool {
    e.kind.is_some() || e.colour.is_some() || e.colours.as_ref().is_some_and(|c| !c.is_empty())
}

fn frame(b: &mut Rig, node: &Node, x: f64, y: f64, w: f64, h: f64, z: f64) {
    let Some(fr) = &node.frame else { return };
    if fr.bezel.is_none() && fr.depth.is_none() && fr.recess.is_none() {
        return;
    }
    let bz = fr.bezel.unwrap_or(0.08);
    let dep = fr.depth.unwrap_or(0.004);
    if node.outline == Some(NodeOutline::Circle) {
        let r = w.min(h) / 2.0;
        b.cyl(
            "pod_back",
            r,
            dep,
            [x, y, z - dep / 2.0],
            Axis::Z,
            M::Body,
            40,
        );
        if bz > 0.0 {
            b.add(
                "pod_bezel",
                mesh::annulus(r, r * (1.0 + bz), dep * 1.5, 40),
                M::Body2,
                tr(x, y, z),
            );
        }
    } else {
        b.box_("group_back", [w, h, dep], [x, y, z - dep / 2.0], M::Body);
        if bz > 0.0 {
            let t = bz * w.min(h);
            for (dx, dy, sw, sh) in [
                (0.0, h / 2.0 + t / 2.0, w + 2.0 * t, t),
                (0.0, -h / 2.0 - t / 2.0, w + 2.0 * t, t),
                (w / 2.0 + t / 2.0, 0.0, t, h),
                (-w / 2.0 - t / 2.0, 0.0, t, h),
            ] {
                b.box_(
                    "group_bezel",
                    [sw, sh, dep * 1.5],
                    [x + dx, y + dy, z],
                    M::Body2,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build(
    b: &mut Rig,
    node: &Node,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    ctx: &mut Ctx,
    cell: Option<u32>,
    key: &mut Vec<usize>,
) -> Res {
    let tilt = node.tilt.as_ref().filter(|t| !t.is_empty());
    let raise = node.z.filter(|&z| z != 0.0);
    if tilt.is_some() || raise.is_some() {
        let (tx, ty) = match tilt.map(Vec::as_slice) {
            None => (0.0, 0.0),
            Some([a, b]) => (*a, *b),
            Some(_) => return fail("node tilt is not [x, y]"),
        };
        let m = tr(x, y, node.z.unwrap_or(0.0)) * ry(ty.to_radians()) * rx(tx.to_radians());
        let flat = Node {
            tilt: None,
            z: None,
            ..node.clone()
        };
        return b.at(m, |b| build(b, &flat, 0.0, 0.0, w, h, ctx, cell, key));
    }
    let leaf = node
        .emitter
        .as_ref()
        .filter(|e| emitter_set(e))
        .filter(|_| node.layout.is_none());
    let (fx, fy) = fill(node, leaf.is_some())?;
    let (mut w, mut h) = (w * fx, h * fy);
    if node.outline == Some(NodeOutline::Circle) {
        w = w.min(h);
        h = w;
    }
    let mut cell = cell;
    if let Some(id) = node.cell_id {
        let c = ctx.c0 as i64 + int(id);
        let c = u32::try_from(c).map_err(|_| crate::rig::BuildError("negative cell_id".into()))?;
        cell = Some(c);
        b.next_cell = b.next_cell.max(c + 1);
    } else if node.cell == Some(CellFlag::Flag(true)) {
        cell = Some(b.next_cell);
        b.next_cell += 1;
    }
    let each = matches!(&node.cell, Some(CellFlag::Mode(m)) if m == "each") || ctx.each;
    frame(b, node, x, y, w, h, ctx.z);
    if let Some(e) = leaf {
        let k = match cell {
            Some(k) if !each => k,
            _ => {
                b.next_cell += 1;
                b.next_cell - 1
            }
        };
        let colour = match e.colours.as_ref().filter(|c| !c.is_empty()) {
            Some(cs) => {
                let i = ctx
                    .colour_i
                    .entry(key[..key.len().saturating_sub(1)].to_vec())
                    .or_insert(0);
                let c = cs[*i % cs.len()].clone();
                *i += 1;
                c
            }
            None => e.colour.clone().unwrap_or_else(|| "rgb".into()),
        };
        let mat = colour_material(&colour);
        b.colour = Some(colour);
        let kind = e.kind.unwrap_or(EmitterKind::Led);
        let zz = ctx.z + 0.0015;
        let shape =
            node.outline
                .unwrap_or(if matches!(kind, EmitterKind::Strip | EmitterKind::Tube) {
                    NodeOutline::Rect
                } else {
                    NodeOutline::Circle
                });
        match shape {
            NodeOutline::Hex => b.emit_cell(k, [x, y, zz], "hex", Size::D(w.min(h)), 0.003, mat),
            NodeOutline::Rect | NodeOutline::Rounded => {
                b.emit_cell(k, [x, y, zz], "square", Size::Wh(w, h), 0.003, mat)
            }
            NodeOutline::Circle => {
                let lens = kind == EmitterKind::Lens;
                if lens {
                    b.cyl(
                        "lens_bezel",
                        w.min(h) * 0.55,
                        0.003,
                        [x, y, ctx.z + 0.001],
                        Axis::Z,
                        M::Body2,
                        16,
                    );
                }
                let lift = if lens { 0.001 } else { 0.0 };
                b.emit_cell(
                    k,
                    [x, y, zz + lift],
                    "circle",
                    Size::D(w.min(h)),
                    0.003,
                    mat,
                );
            }
        }
        b.colour = None;
        return Ok(());
    }
    let cover = match &node.diffuser {
        Some(Diffuser::Flag(true)) => Some(0.006),
        Some(Diffuser::Cover { z: Some(z) }) => Some(*z),
        _ => None,
    };
    if let Some(dz) = cover {
        if node.outline == Some(NodeOutline::Circle) {
            b.cyl(
                "diffuser",
                w.min(h) / 2.0,
                0.002,
                [x, y, ctx.z + dz],
                Axis::Z,
                M::Diffuser,
                40,
            );
        } else {
            b.box_("diffuser", [w, h, 0.002], [x, y, ctx.z + dz], M::Diffuser);
        }
    }
    let kids = node.children.as_ref().filter(|k| !k.is_empty());
    let clip = node.outline == Some(NodeOutline::Circle)
        && matches!(
            node.layout.as_ref().map(|l| l.kind),
            Some(LayoutKind::Grid | LayoutKind::Line)
        );
    if let (Some(k), Some(led), false, false) = (cell, panel_led(node), each, clip) {
        return led_panel(b, node, led, x, y, w, h, ctx.z, k);
    }
    let old = ctx.each;
    ctx.each = each;
    for (i, (sx, sy, sw, sh)) in slots(node, w, h).into_iter().enumerate() {
        let child = match kids {
            Some(k) => Some(&k[i % k.len()]),
            None => node.child.as_deref(),
        };
        let Some(child) = child else { continue };
        if clip && !inside(Some(NodeOutline::Circle), sx, sy, w, h) {
            continue;
        }
        key.push(i);
        let r = build(b, child, x + sx, y + sy, sw, sh, ctx, cell, key);
        key.pop();
        r?;
    }
    ctx.each = old;
    Ok(())
}

/// A node's `fill`, `(fx, fy)`.
fn fill(node: &Node, leaf: bool) -> Res<(f64, f64)> {
    let default = if leaf { 0.9 } else { 1.0 };
    match &node.fill {
        None => Ok((default, default)),
        Some(crate::spec::NumOrList::Num(f)) => Ok((*f, *f)),
        Some(crate::spec::NumOrList::List(f)) if f.len() >= 2 => Ok((f[0], f[1])),
        Some(_) => fail("node fill is not a number or [fx, fy]"),
    }
}

/// Fewest LEDs a flat panel is drawn as one face for.
const PANEL_LEDS: usize = 4;

/// The one LED every slot of `node` holds, when `node` is a flat LED panel:
/// a grid or line of at least [`PANEL_LEDS`] identical flat LEDs that light
/// one cell. A lens has relief (a bezel, a raised dome), and so does an LED
/// tilted, raised or framed; those stay meshes.
fn panel_led(node: &Node) -> Option<&Node> {
    let layout = node.layout.as_ref()?;
    if !matches!(layout.kind, LayoutKind::Grid | LayoutKind::Line) {
        return None;
    }
    let led = match node.children.as_ref().filter(|k| !k.is_empty()) {
        Some(kids) => kids.iter().all(|k| *k == kids[0]).then(|| &kids[0])?,
        None => node.child.as_deref()?,
    };
    let e = led.emitter.as_ref().filter(|e| emitter_set(e))?;
    let flat = led.layout.is_none()
        && e.kind != Some(EmitterKind::Lens)
        && e.colours.as_ref().is_none_or(Vec::is_empty)
        && led.tilt.as_ref().is_none_or(Vec::is_empty)
        && led.z.is_none_or(|z| z == 0.0)
        && led.frame.is_none()
        && !has_cells(led);
    let count = match layout.kind {
        LayoutKind::Grid => {
            int(layout.rows.unwrap_or(1.0)).max(1) * int(layout.cols.unwrap_or(1.0)).max(1)
        }
        _ => int(layout.n.unwrap_or(1.0)).max(1),
    };
    (flat && count as usize >= PANEL_LEDS).then_some(led)
}

/// A flat LED panel of cell `k` filling `w x h` at `(x, y)`: one face with
/// the pattern of its LEDs, and every LED listed among the cell's emitters
/// exactly where a mesh per LED would put it.
#[allow(clippy::too_many_arguments)]
fn led_panel(
    b: &mut Rig,
    node: &Node,
    led: &Node,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    z: f64,
    k: u32,
) -> Res {
    let l = node.layout.as_ref().expect("a panel has a layout");
    let stagger = l.kind == LayoutKind::Grid && l.stagger.unwrap_or(false);
    let (rows, cols) = match l.kind {
        LayoutKind::Grid => (
            int(l.rows.unwrap_or(1.0)).max(1),
            int(l.cols.unwrap_or(1.0)).max(1),
        ),
        _ => {
            let n = int(l.n.unwrap_or(1.0)).max(1);
            match l.dir.unwrap_or(if w >= h { Dir::X } else { Dir::Y }) {
                Dir::X => (1, n),
                Dir::Y => (n, 1),
            }
        }
    };
    let span = [cols as f64 + if stagger { 0.5 } else { 0.0 }, rows as f64];
    let (px, py) = (w / span[0], h / span[1]);
    let slots = slots(node, w, h);
    let (fx, fy) = fill(led, true)?;
    let (lw, lh) = (slots[0].2 * fx, slots[0].3 * fy);
    let d = lw.min(lh);
    let e = led.emitter.as_ref().expect("a panel LED emits");
    let strip = matches!(e.kind, Some(EmitterKind::Strip | EmitterKind::Tube));
    let default = if strip {
        NodeOutline::Rect
    } else {
        NodeOutline::Circle
    };
    let (shape, size, outline, [ew, eh]) = match led.outline.unwrap_or(default) {
        NodeOutline::Rect | NodeOutline::Rounded => {
            ("square", Size::Wh(lw, lh), EmitterShape::Rect, [lw, lh])
        }
        NodeOutline::Hex => ("hex", Size::D(d), EmitterShape::Hex, [d, d]),
        NodeOutline::Circle => ("circle", Size::D(d), EmitterShape::Circle, [d, d]),
    };
    let colour = e.colour.clone().unwrap_or_else(|| "rgb".into());
    let mat = colour_material(&colour);
    b.colour = Some(colour);
    let zz = z + 0.0015;
    for (sx, sy, _, _) in slots {
        b.panel_emitter(k, [x + sx, y + sy, zz], shape, size, mat);
    }
    b.colour = None;
    let share = [(ew / px).min(1.0), (eh / py).min(1.0)];
    let pattern = LedPattern {
        shape: outline,
        fill: share.map(|f| f as f32),
        cols: cols as u32,
        stagger,
        gap: M::Body,
    };
    // The face reaches half a gap past the outermost LEDs, so neighbouring
    // panels meet with no seam. Along a single row (or column) the slot can
    // be far taller than its LEDs; there it reaches the row's half gap.
    let half_gap = [(1.0 - share[0]) / 2.0 * px, (1.0 - share[1]) / 2.0 * py];
    let reach = |axis: usize, leds: i64| {
        if leds > 1 {
            half_gap[axis]
        } else {
            half_gap[0].min(half_gap[1])
        }
    };
    let trim = [half_gap[0] - reach(0, cols), half_gap[1] - reach(1, rows)];
    let uv0 = [trim[0] / px, trim[1] / py];
    b.led_panel(
        k,
        [x, y, zz + 0.0015],
        w - 2.0 * trim[0],
        h - 2.0 * trim[1],
        [uv0, [span[0] - uv0[0], span[1] - uv0[1]]],
        pattern,
        mat,
    );
    Ok(())
}

fn has_cells(n: &Node) -> bool {
    let flagged = match &n.cell {
        Some(CellFlag::Flag(f)) => *f,
        Some(CellFlag::Mode(m)) => !m.is_empty(),
        None => false,
    };
    flagged
        || n.cell_id.is_some()
        || n.children.iter().flatten().any(has_cells)
        || n.child.as_deref().is_some_and(has_cells)
}

/// A face root on the face plane (`fw x fh` at depth `z`, centred), with one
/// source over every cell it made.
pub(crate) fn build_face(b: &mut Rig, root: &Node, g: Geom) -> Res {
    let rounded;
    let root = if g.rnd && root.outline.is_none() {
        rounded = Node {
            outline: Some(NodeOutline::Circle),
            ..root.clone()
        };
        &rounded
    } else {
        root
    };
    let c0 = b.next_cell;
    let mut ctx = Ctx {
        z: g.z + 0.0024,
        c0,
        each: false,
        colour_i: HashMap::new(),
    };
    let mut face_cell = None;
    if !has_cells(root) {
        face_cell = Some(c0);
        b.next_cell += 1;
    }
    build(
        b,
        root,
        0.0,
        0.0,
        g.fw,
        g.fh,
        &mut ctx,
        face_cell,
        &mut Vec::new(),
    )?;
    if b.next_cell == c0 {
        b.next_cell += 1;
    }
    b.source(
        DVec3::new(0.0, 0.0, g.z + 0.008),
        (c0..b.next_cell).collect(),
    );
    Ok(())
}
