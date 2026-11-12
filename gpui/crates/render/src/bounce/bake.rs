//! The bounce bake: what of each surface's light lands on each other, worked
//! out once for a stage's geometry and kept until it changes. No GPU: it
//! runs on a thread while the frames go on without it.
//!
//! - **Discs** ([`discs`]): every big surface cut into pieces about
//!   `spacing` across, a disc to a piece. A disc is where light is taken
//!   in, and where it is sent back out.
//! - **Patches and groups** ([`cluster`]): neighbouring discs on one
//!   surface with nothing between them make a patch, and patches make a
//!   group. Far away a whole group looks like one light.
//! - **Links** ([`links`]): for each receiver, a disc or a point in the air,
//!   the share of each emitter's light that lands on it. One ray to each
//!   disc says whether it is seen. Then the coarsest emitters that say the
//!   same thing are kept: a group where its discs all weigh what their size
//!   says they should, its patches where they do not, and discs one by one
//!   where a patch is close or half hidden. A point in the air keeps one
//!   list for all six of its faces, so its emitters must also lie in one
//!   direction.
//! - **Clip planes** ([`clips`]): the faces that stand beside a disc, so a
//!   point on the other side of a barrier does not take the disc's light.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use bytemuck::{Pod, Zeroable};
use glam::{IVec3, UVec3, Vec3};
use luma_scene::bvh::{Ray, TriMesh};

/// How far off a surface a ray starts and ends, in metres.
const LIFT: f32 = 0.02;

/// One triangle of a surface that carries discs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Face {
    pub corners: [Vec3; 3],
    /// The matte colour: none for metal.
    pub matte: Vec3,
    /// What it mirrors head-on.
    pub f0: Vec3,
    pub roughness: f32,
    /// Whether it is the ground, which outdoors the sky probe already holds
    /// lit by the sun and the sky.
    pub ground: bool,
}

/// `BounceDisc` in `bounce_common.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct Disc {
    pub pos: [f32; 3],
    /// Of the disc direct light is taken over.
    pub radius: f32,
    pub normal: [f32; 3],
    /// The surface it stands for, in square metres.
    pub area: f32,
    pub matte: [f32; 3],
    pub patch: u32,
    pub f0: [f32; 3],
    pub roughness: f32,
    /// Up to two planes, each facing the disc: a point behind one is on the
    /// other side of something. A plane with no normal clips nothing.
    pub clip: [[f32; 4]; 2],
    /// How far round itself it speaks for the surface.
    pub reach: f32,
    /// 1 on the ground ([`Face::ground`]).
    pub ground: f32,
    pub _pad: [f32; 2],
}

/// `BouncePatch` in `bounce_common.wgsl`: a run of discs.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct Patch {
    pub start: u32,
    pub count: u32,
    pub group: u32,
    pub area: f32,
}

/// `BounceGroup` in `bounce_common.wgsl`: a run of patches.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct Group {
    pub start: u32,
    pub count: u32,
    pub area: f32,
    pub _pad: f32,
}

/// `BounceLink` in `bounce_common.wgsl`. `node` counts discs, then patches,
/// then groups.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct Link {
    pub node: u32,
    /// The share of the emitter's light that lands on a disc's face. A
    /// point in the air has no one face: there it is `size`, and each face
    /// takes its share by `dir`.
    pub weight: f32,
    /// The emitter's size in the receiver's sky.
    pub size: f32,
    /// Where it is, as [`pack_direction`] packs it.
    pub dir: u32,
}

/// What a bake is asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Settings {
    /// Metres between discs.
    pub spacing: f32,
    /// How much of a receiver's light one link may get wrong before the
    /// emitter is taken in smaller parts.
    pub error: f32,
}

/// A stage's bounce, baked.
#[derive(Clone, Debug, Default)]
pub(crate) struct Baked {
    pub discs: Vec<Disc>,
    pub patches: Vec<Patch>,
    pub groups: Vec<Group>,
    /// Where each receiver's links start: every disc, then every air
    /// point. One more than the receivers.
    pub starts: Vec<u32>,
    pub links: Vec<Link>,
}

/// A unit direction in 32 bits (octahedral); `unpack_direction` in
/// `bounce_common.wgsl` reads it.
pub(crate) fn pack_direction(d: Vec3) -> u32 {
    let n = d / (d.x.abs() + d.y.abs() + d.z.abs()).max(1e-9);
    let (mut x, mut y) = (n.x, n.y);
    if n.z < 0.0 {
        let sign = |v: f32| if v >= 0.0 { 1.0 } else { -1.0 };
        (x, y) = ((1.0 - n.y.abs()) * sign(n.x), (1.0 - n.x.abs()) * sign(n.y));
    }
    let q = |v: f32| ((v * 0.5 + 0.5).clamp(0.0, 1.0) * 65535.0).round() as u32;
    q(x) | (q(y) << 16)
}

/// A piece of a triangle that one disc will stand for.
#[derive(Clone, Copy)]
struct Piece {
    corners: [Vec3; 3],
    face: usize,
    /// The area it answers for: more than its own when it is one sliver
    /// kept to stand for several.
    stands: f32,
}

impl Piece {
    fn area(&self) -> f32 {
        let [a, b, c] = self.corners;
        (b - a).cross(c - a).length() * 0.5
    }

    /// The longest edge's ends, then the corner opposite.
    fn longest(&self) -> (Vec3, Vec3, Vec3) {
        let [a, b, c] = self.corners;
        [(a, b, c), (b, c, a), (c, a, b)]
            .into_iter()
            .max_by(|x, y| x.0.distance(x.1).total_cmp(&y.0.distance(y.1)))
            .unwrap_or((a, b, c))
    }
}

/// A fixed sequence of numbers in `0..1`, so a stage bakes the same twice.
struct Sequence(u64);

impl Sequence {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
}

/// A disc for every piece of every face: each triangle halved across its
/// longest edge until its pieces are about `spacing` across. A piece smaller
/// still (one facet of a tube) is kept by chance, so slivers add up to their
/// area; a long thin one (a slat's edge) is all there is along its length,
/// and is kept.
pub(crate) fn discs(faces: &[Face], spacing: f32) -> Vec<Disc> {
    let unit = spacing * spacing;
    let floor = unit / 3.0;
    let mut sequence = Sequence(1);
    let mut out = Vec::new();
    for (index, face) in faces.iter().enumerate() {
        let normal = (face.corners[1] - face.corners[0])
            .cross(face.corners[2] - face.corners[0])
            .normalize_or_zero();
        if normal == Vec3::ZERO {
            continue;
        }
        let mut stack = vec![Piece {
            corners: face.corners,
            face: index,
            stands: 0.0,
        }];
        while let Some(mut piece) = stack.pop() {
            let area = piece.area();
            let (p, q, r) = piece.longest();
            let long = p.distance(q);
            if area > unit * 0.75 || long > spacing * 2.0 {
                let middle = (p + q) * 0.5;
                for corners in [[p, middle, r], [middle, q, r]] {
                    stack.push(Piece { corners, ..piece });
                }
                continue;
            }
            piece.stands = area;
            if area < floor && long < spacing {
                if sequence.next() >= area / floor {
                    continue;
                }
                piece.stands = floor;
            }
            // Light is taken across a disc no wider than the piece.
            let width = 2.0 * area / long.max(1e-6);
            let face = &faces[piece.face];
            out.push(Disc {
                pos: ((p + q + r) / 3.0).to_array(),
                radius: (piece.stands / std::f32::consts::PI)
                    .sqrt()
                    .min(width * 0.5),
                normal: normal.to_array(),
                area: piece.stands,
                matte: face.matte.to_array(),
                patch: 0,
                f0: face.f0.to_array(),
                roughness: face.roughness,
                clip: [[0.0; 4]; 2],
                reach: (1.6 * piece.stands.sqrt()).max(0.65 * long),
                ground: f32::from(u8::from(face.ground)),
                _pad: [0.0; 2],
            });
        }
    }
    out
}

fn pos(disc: &Disc) -> Vec3 {
    Vec3::from(disc.pos)
}

fn normal(disc: &Disc) -> Vec3 {
    Vec3::from(disc.normal)
}

/// Each disc's neighbours on its own surface: within either's reach, facing
/// the same way and in the same plane.
fn neighbours(discs: &[Disc]) -> Vec<Vec<u32>> {
    let cell = discs.iter().map(|d| d.reach).fold(0.1, f32::max);
    let key = |p: Vec3| (p / cell).floor().as_ivec3();
    let mut cells: HashMap<IVec3, Vec<u32>> = HashMap::new();
    for (i, disc) in discs.iter().enumerate() {
        cells.entry(key(pos(disc))).or_default().push(i as u32);
    }
    discs
        .iter()
        .enumerate()
        .map(|(i, disc)| {
            let (p, n) = (pos(disc), normal(disc));
            let home = key(p);
            let mut found = Vec::new();
            for dz in -1..=1 {
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let Some(list) = cells.get(&(home + IVec3::new(dx, dy, dz))) else {
                            continue;
                        };
                        for &j in list {
                            let other = &discs[j as usize];
                            let gap = pos(other) - p;
                            let reach = disc.reach.max(other.reach);
                            if j as usize != i
                                && gap.length_squared() <= reach * reach
                                && normal(other).dot(n) >= 0.9
                                && gap.dot(n).abs() <= 0.1
                            {
                                found.push(j);
                            }
                        }
                    }
                }
            }
            found
        })
        .collect()
}

/// Disjoint sets, for joining discs that are neighbours.
struct Sets(Vec<u32>);

impl Sets {
    fn root(&mut self, mut i: u32) -> u32 {
        while self.0[i as usize] != i {
            self.0[i as usize] = self.0[self.0[i as usize] as usize];
            i = self.0[i as usize];
        }
        i
    }

    fn join(&mut self, a: u32, b: u32) {
        let (a, b) = (self.root(a), self.root(b));
        self.0[a.max(b) as usize] = a.min(b);
    }
}

/// Sort `discs` into patches about `size` across, and the patches into
/// groups four times that. A patch is neighbours on one surface that
/// nothing stands between, so no patch has a part on each side of a wall.
/// Small things (a slat's faces) share the group of their place: a group
/// each would be a link each from every receiver that sees them.
pub(crate) fn cluster(
    discs: &mut Vec<Disc>,
    blockers: &TriMesh,
    size: f32,
) -> (Vec<Patch>, Vec<Group>) {
    let place = |disc: &Disc, size: f32| {
        let n = normal(disc);
        let a = n.abs();
        let axis = if a.x >= a.y && a.x >= a.z {
            0
        } else if a.y >= a.z {
            1
        } else {
            2
        };
        let cell = (pos(disc) / size).floor().as_ivec3();
        (
            axis * 2 + usize::from(n[axis] < 0.0),
            cell.x,
            cell.y,
            cell.z,
        )
    };
    let count = discs.len() as u32;
    let near = neighbours(discs);
    // The neighbours nothing stands between.
    let friends: Vec<Vec<u32>> = near
        .iter()
        .enumerate()
        .map(|(i, list)| {
            let lift = normal(&discs[i]) * 0.05;
            let from = pos(&discs[i]) + lift;
            list.iter()
                .copied()
                .filter(|&j| {
                    let to = pos(&discs[j as usize]) + lift - from;
                    let far = to.length();
                    far <= 1e-6 || !blockers.blocked(Ray::new(from, to), far)
                })
                .collect()
        })
        .collect();
    let joined = |size: f32| {
        let mut sets = Sets((0..count).collect());
        for (i, list) in friends.iter().enumerate() {
            for &j in list {
                if place(&discs[i], size) == place(&discs[j as usize], size) {
                    sets.join(i as u32, j);
                }
            }
        }
        (0..count).map(|i| sets.root(i)).collect::<Vec<u32>>()
    };
    let (small, mut big) = (joined(size), joined(size * 4.0));
    let mut area = vec![0.0f32; discs.len()];
    for (i, disc) in discs.iter().enumerate() {
        area[big[i] as usize] += disc.area;
    }
    for root in &mut big {
        if area[*root as usize] < size * size * 2.0 {
            *root = u32::MAX;
        }
    }
    let sort_key = |i: usize| {
        (
            place(&discs[i], size * 4.0),
            big[i],
            place(&discs[i], size),
            small[i],
        )
    };
    let mut order: Vec<usize> = (0..discs.len()).collect();
    order.sort_by_key(|&i| sort_key(i));
    let keys: Vec<_> = order.iter().map(|&i| sort_key(i)).collect();
    *discs = order.iter().map(|&i| discs[i]).collect();

    let mut patches: Vec<Patch> = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    for at in 0..keys.len() {
        let new_group = at == 0 || (keys[at].0, keys[at].1) != (keys[at - 1].0, keys[at - 1].1);
        if new_group {
            groups.push(Group {
                start: patches.len() as u32,
                count: 0,
                area: 0.0,
                _pad: 0.0,
            });
        }
        let group = groups.len() - 1;
        if new_group || keys[at] != keys[at - 1] {
            patches.push(Patch {
                start: at as u32,
                count: 0,
                group: group as u32,
                area: 0.0,
            });
            groups[group].count += 1;
        }
        let patch = patches.len() - 1;
        discs[at].patch = patch as u32;
        patches[patch].count += 1;
        patches[patch].area += discs[at].area;
        groups[group].area += discs[at].area;
    }
    (patches, groups)
}

/// Each disc's clip planes: the faces that stand between it and its
/// neighbours on the same surface (a barrier across a floor), the nearest
/// two. A point past such a face is on the other side of something, and
/// the disc says nothing about it.
pub(crate) fn clips(discs: &mut [Disc], blockers: &TriMesh) {
    let near = neighbours(discs);
    for (i, list) in near.iter().enumerate() {
        let lift = normal(&discs[i]) * 0.05;
        let from = pos(&discs[i]) + lift;
        let mut planes = [[0.0f32; 4]; 2];
        let mut nearest = [f32::INFINITY; 2];
        for &j in list {
            let to = pos(&discs[j as usize]) + lift - from;
            let far = to.length();
            if far <= 1e-6 {
                continue;
            }
            let ray = Ray::new(from, to);
            let Some(hit) = blockers.raycast(ray).filter(|hit| hit.t < far) else {
                continue;
            };
            // Turned toward this disc.
            let n = if hit.normal.dot(ray.dir) > 0.0 {
                -hit.normal
            } else {
                hit.normal
            };
            let d = n.dot(hit.point);
            let same = |plane: &[f32; 4]| {
                Vec3::new(plane[0], plane[1], plane[2]).dot(n) > 0.9 && (plane[3] - d).abs() < 0.05
            };
            if (nearest[0].is_finite() && same(&planes[0]))
                || (nearest[1].is_finite() && same(&planes[1]))
            {
                continue;
            }
            let plane = [n.x, n.y, n.z, d];
            if hit.t < nearest[0] {
                planes[1] = planes[0];
                nearest[1] = nearest[0];
                planes[0] = plane;
                nearest[0] = hit.t;
            } else if hit.t < nearest[1] {
                planes[1] = plane;
                nearest[1] = hit.t;
            }
        }
        discs[i].clip = planes;
    }
}

/// A place light is taken in: a point, and the face it is taken on, or
/// none for a point in the air, which takes it from every side.
struct Receiver {
    at: Vec3,
    face: Option<Vec3>,
    /// The disc it is, which sends itself nothing.
    own: Option<usize>,
}

/// One receiver's view of every disc, reused from receiver to receiver.
#[derive(Default)]
struct Seen {
    /// Each disc's size in the receiver's sky, nothing where it is hidden
    /// or faces away.
    size: Vec<f32>,
    dir: Vec<Vec3>,
    /// Each disc's weight for the receiver in hand: its share of the
    /// disc's light on a face, its size for a point in the air.
    weight: Vec<f32>,
}

/// The emitters `baked` sorted its discs into, and how to walk them.
struct Emitters<'a> {
    discs: &'a [Disc],
    patches: &'a [Patch],
    groups: &'a [Group],
    blockers: &'a TriMesh,
    error: f32,
}

impl Emitters<'_> {
    /// How big each disc is in the sky over `receiver`, and where.
    fn look(&self, receiver: &Receiver, seen: &mut Seen) {
        seen.size.clear();
        seen.dir.clear();
        for (index, disc) in self.discs.iter().enumerate() {
            let mut size = 0.0;
            let mut dir = Vec3::ZERO;
            if receiver.own != Some(index) {
                let n = normal(disc);
                let to = pos(disc) + n * LIFT - receiver.at;
                let r2 = to.length_squared();
                let far = r2.sqrt();
                dir = to / far.max(1e-9);
                let facing = -n.dot(dir);
                if facing > 0.0
                    && receiver.face.is_none_or(|face| face.dot(dir) > 0.0)
                    && !self.blockers.blocked(
                        Ray {
                            origin: receiver.at,
                            dir,
                        },
                        far - 0.01,
                    )
                {
                    // A disc's form factor, which stays finite close up.
                    size = facing * disc.area / (std::f32::consts::PI * r2 + disc.area);
                }
            }
            seen.size.push(size);
            seen.dir.push(dir);
        }
    }

    /// A receiver's links, appended to `out`: those of a disc's `face`, or
    /// with none those of a point in the air, which weighs each emitter by
    /// its size and leaves the facing to each of its six faces.
    fn links(&self, face: Option<Vec3>, seen: &mut Seen, out: &mut Vec<Link>) {
        seen.weight.clear();
        seen.weight.extend(
            seen.size
                .iter()
                .zip(&seen.dir)
                .map(|(size, dir)| size * face.map_or(1.0, |face| face.dot(*dir).max(0.0))),
        );
        let total: f32 = seen.weight.iter().sum();
        if total <= 0.0 {
            return;
        }
        // The air's light is spread over a truss or a lamp's body, which
        // shows less of it than a wall does.
        let allowed = self.error * total * if face.is_some() { 1.0 } else { AIR_ERROR };
        let floor = allowed * 0.05;
        let (disc_count, patch_count) = (self.discs.len(), self.patches.len());
        let mut link = |node: usize, range: std::ops::Range<usize>| {
            let weight: f32 = seen.weight[range.clone()].iter().sum();
            if weight < floor {
                return;
            }
            let mut size = 0.0;
            let mut dir = Vec3::ZERO;
            for d in range {
                if seen.weight[d] > 0.0 {
                    size += seen.size[d];
                    dir += seen.dir[d] * seen.size[d];
                }
            }
            out.push(Link {
                node: node as u32,
                weight,
                size,
                dir: pack_direction(dir.normalize_or_zero()),
            });
        };
        // How far the discs' weights are from what one link to all of them
        // says: each disc's share of the whole by its area. For the air,
        // discs that lie in different directions stray by that too: one
        // link has one direction.
        let strays = |range: std::ops::Range<usize>, area: f32| {
            let weight: f32 = seen.weight[range.clone()].iter().sum();
            let mut stray: f32 = range
                .clone()
                .map(|d| (seen.weight[d] - weight * self.discs[d].area / area).abs())
                .sum();
            if face.is_none() {
                let toward: Vec3 = range.map(|d| seen.dir[d] * seen.weight[d]).sum();
                stray += weight - toward.length();
            }
            (weight, stray)
        };
        let run = |patch: &Patch| patch.start as usize..(patch.start + patch.count) as usize;
        for (g, group) in self.groups.iter().enumerate() {
            let patches = &self.patches[group.start as usize..(group.start + group.count) as usize];
            let (Some(first), Some(last)) = (patches.first(), patches.last()) else {
                continue;
            };
            let all = first.start as usize..(last.start + last.count) as usize;
            let (weight, stray) = strays(all.clone(), group.area);
            if weight <= 0.0 {
                continue;
            }
            if stray <= allowed {
                link(disc_count + patch_count + g, all);
                continue;
            }
            for (p, patch) in patches.iter().enumerate() {
                let (weight, stray) = strays(run(patch), patch.area);
                if weight <= 0.0 {
                    continue;
                }
                if stray <= allowed * 0.25 {
                    link(disc_count + group.start as usize + p, run(patch));
                } else {
                    for d in run(patch) {
                        link(d, d..d + 1);
                    }
                }
            }
        }
    }
}

/// How much more of its light a link to a point in the air may get wrong
/// than a link to a disc.
const AIR_ERROR: f32 = 2.0;

/// Receivers a thread takes at a time.
const RUN: usize = 32;

/// Every receiver's links: the discs', then those of each point of `air`.
/// `None` when `stop` was raised before the end.
fn links(
    emitters: &Emitters<'_>,
    air: &[Vec3],
    stop: &AtomicBool,
) -> Option<(Vec<u32>, Vec<Link>)> {
    let disc_count = emitters.discs.len();
    let receivers = disc_count + air.len();
    let runs = receivers.div_ceil(RUN);
    let next = AtomicUsize::new(0);
    // Each run's links and how many each of its receivers has.
    let done: Mutex<Vec<Option<(Vec<u32>, Vec<Link>)>>> = Mutex::new(vec![None; runs]);
    // One core is left to the frames that go on meanwhile.
    let threads = std::thread::available_parallelism()
        .map_or(1, |cores| cores.get().saturating_sub(1).max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut seen = Seen::default();
                loop {
                    let run = next.fetch_add(1, Ordering::Relaxed);
                    if run >= runs || stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut counts = Vec::new();
                    let mut out = Vec::new();
                    for index in run * RUN..((run + 1) * RUN).min(receivers) {
                        let before = out.len();
                        if index < disc_count {
                            let disc = &emitters.discs[index];
                            let face = normal(disc);
                            let receiver = Receiver {
                                at: pos(disc) + face * LIFT,
                                face: Some(face),
                                own: Some(index),
                            };
                            emitters.look(&receiver, &mut seen);
                            emitters.links(receiver.face, &mut seen, &mut out);
                        } else {
                            let receiver = Receiver {
                                at: air[index - disc_count],
                                face: None,
                                own: None,
                            };
                            emitters.look(&receiver, &mut seen);
                            emitters.links(None, &mut seen, &mut out);
                        }
                        counts.push((out.len() - before) as u32);
                    }
                    let mut done = done
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    done[run] = Some((counts, out));
                }
            });
        }
    });
    if stop.load(Ordering::Relaxed) {
        return None;
    }
    let mut starts = vec![0u32];
    let mut all = Vec::new();
    for run in done
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        let (counts, links) = run?;
        for count in counts {
            starts.push(starts[starts.len() - 1] + count);
        }
        all.extend(links);
    }
    Some((starts, all))
}

/// Bake `faces`, among `blockers`, for themselves and for the `air` points.
/// `None` when `stop` was raised before the end.
pub(crate) fn bake(
    faces: &[Face],
    blockers: &TriMesh,
    air: &[Vec3],
    settings: Settings,
    stop: &AtomicBool,
) -> Option<Baked> {
    let mut discs = discs(faces, settings.spacing);
    let (patches, groups) = cluster(&mut discs, blockers, settings.spacing * 3.0);
    clips(&mut discs, blockers);
    let emitters = Emitters {
        discs: &discs,
        patches: &patches,
        groups: &groups,
        blockers,
        error: settings.error,
    };
    let (starts, links) = links(&emitters, air, stop)?;
    Some(Baked {
        discs,
        patches,
        groups,
        starts,
        links,
    })
}

/// The grid that finds the discs round a point: each cell lists the discs
/// whose reach touches it. Returns each cell's start in the list, one more
/// than the cells, and the list.
pub(crate) fn grid(discs: &[Disc], lo: Vec3, dims: UVec3, cell: f32) -> (Vec<u32>, Vec<u32>) {
    let cells = (dims.x * dims.y * dims.z) as usize;
    let touched = |disc: &Disc| {
        let p = pos(disc);
        let reach = disc.reach;
        let a = ((p - reach - lo) / cell)
            .floor()
            .max(Vec3::ZERO)
            .as_uvec3()
            .min(dims - 1);
        let b = ((p + reach - lo) / cell)
            .floor()
            .max(Vec3::ZERO)
            .as_uvec3()
            .min(dims - 1);
        (a.z..=b.z)
            .flat_map(move |z| (a.y..=b.y).flat_map(move |y| (a.x..=b.x).map(move |x| (x, y, z))))
            .filter_map(move |(x, y, z)| {
                // The sphere against the cell's box.
                let low = lo + UVec3::new(x, y, z).as_vec3() * cell;
                let nearest = p.clamp(low, low + cell);
                (nearest.distance(p) <= reach).then_some((x + dims.x * (y + dims.y * z)) as usize)
            })
    };
    let mut start = vec![0u32; cells + 1];
    for disc in discs {
        for c in touched(disc) {
            start[c + 1] += 1;
        }
    }
    for i in 0..cells {
        start[i + 1] += start[i];
    }
    let mut cursor = start.clone();
    let mut items = vec![0u32; start[cells] as usize];
    for (i, disc) in discs.iter().enumerate() {
        for c in touched(disc) {
            items[cursor[c] as usize] = i as u32;
            cursor[c] += 1;
        }
    }
    (start, items)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Vec3 = Vec3::splat(0.5);

    /// A quad as two faces, wound so its normal is `(b - a) x (d - a)`.
    fn quad(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> [Face; 2] {
        [[a, b, c], [a, c, d]].map(|corners| Face {
            corners,
            matte: WHITE,
            f0: Vec3::splat(0.04),
            roughness: 0.9,
            ground: false,
        })
    }

    /// The inside of a box from `lo` to `hi`.
    fn room(lo: Vec3, hi: Vec3) -> Vec<Face> {
        let p = |x: bool, y: bool, z: bool| {
            Vec3::new(
                if x { hi.x } else { lo.x },
                if y { hi.y } else { lo.y },
                if z { hi.z } else { lo.z },
            )
        };
        let centre = (lo + hi) * 0.5;
        let mut faces = Vec::new();
        for [a, b, c, d] in [
            [
                p(false, false, false),
                p(false, true, false),
                p(false, true, true),
                p(false, false, true),
            ],
            [
                p(true, false, false),
                p(true, true, false),
                p(true, true, true),
                p(true, false, true),
            ],
            [
                p(false, false, false),
                p(true, false, false),
                p(true, false, true),
                p(false, false, true),
            ],
            [
                p(false, true, false),
                p(true, true, false),
                p(true, true, true),
                p(false, true, true),
            ],
            [
                p(false, false, false),
                p(true, false, false),
                p(true, true, false),
                p(false, true, false),
            ],
            [
                p(false, false, true),
                p(true, false, true),
                p(true, true, true),
                p(false, true, true),
            ],
        ] {
            let inward = (b - a).cross(d - a).dot(centre - a) > 0.0;
            faces.extend(if inward {
                quad(a, b, c, d)
            } else {
                quad(a, d, c, b)
            });
        }
        faces
    }

    fn mesh(faces: &[Face]) -> TriMesh {
        let positions: Vec<Vec3> = faces.iter().flat_map(|face| face.corners).collect();
        let triangles = (0..faces.len() as u32)
            .map(|i| [i * 3, i * 3 + 1, i * 3 + 2])
            .collect();
        TriMesh::new(positions, triangles)
    }

    fn baked(faces: &[Face], air: &[Vec3], spacing: f32) -> Baked {
        bake(
            faces,
            &mesh(faces),
            air,
            Settings {
                spacing,
                error: 0.01,
            },
            &AtomicBool::new(false),
        )
        .expect("not stopped")
    }

    /// Each receiver's links' weights, summed.
    fn totals(baked: &Baked) -> Vec<f32> {
        baked
            .starts
            .windows(2)
            .map(|w| {
                baked.links[w[0] as usize..w[1] as usize]
                    .iter()
                    .map(|l| l.weight)
                    .sum()
            })
            .collect()
    }

    /// The discs a link's node stands for.
    fn members(baked: &Baked, node: u32) -> std::ops::Range<usize> {
        let (discs, patches) = (baked.discs.len(), baked.patches.len());
        let node = node as usize;
        let patch = |p: &Patch| p.start as usize..(p.start + p.count) as usize;
        if node < discs {
            node..node + 1
        } else if node < discs + patches {
            patch(&baked.patches[node - discs])
        } else {
            let group = &baked.groups[node - discs - patches];
            let first = patch(&baked.patches[group.start as usize]);
            let last = patch(&baked.patches[(group.start + group.count - 1) as usize]);
            first.start..last.end
        }
    }

    /// In a closed room everything a surface sees is another surface, so
    /// the shares of light landing on a disc add up to all of it: none is
    /// lost to the sorting into patches and groups, and none is made.
    #[test]
    fn a_closed_room_loses_no_light() {
        let faces = room(Vec3::ZERO, Vec3::new(8.0, 6.0, 4.0));
        let baked = baked(&faces, &[Vec3::new(4.0, 3.0, 2.0)], 0.5);
        let area: f32 = baked.discs.iter().map(|d| d.area).sum();
        assert!(
            (area - 208.0).abs() < 1.0,
            "the discs stand for the room's {area} m2"
        );
        let totals = totals(&baked);
        let mean = totals[..baked.discs.len()].iter().sum::<f32>() / baked.discs.len() as f32;
        assert!(
            (mean - 1.0).abs() < 0.05,
            "a disc takes {mean} of the room's light"
        );
        // A point in the air has the whole room round it, and counts it by
        // size, not at a slant: four times what a face takes.
        for point in &totals[baked.discs.len()..] {
            assert!((point - 4.0).abs() < 0.2, "an air point takes {point}");
        }
    }

    /// Far surfaces are taken a group at a time: a disc has far fewer links
    /// than there are discs.
    #[test]
    fn far_surfaces_are_taken_together() {
        let faces = room(Vec3::ZERO, Vec3::new(20.0, 14.0, 7.0));
        let baked = baked(&faces, &[], 0.5);
        let per = baked.links.len() as f32 / baked.discs.len() as f32;
        assert!(
            per < baked.discs.len() as f32 / 20.0,
            "{per} links a disc of {}",
            baked.discs.len()
        );
        assert!(baked.groups.len() < baked.patches.len());
        assert!(baked.patches.len() < baked.discs.len());
    }

    /// A wall across a room: nothing on one side takes light from the
    /// other, and no patch has discs on both sides.
    #[test]
    fn a_wall_parts_the_light() {
        let mut faces = room(Vec3::ZERO, Vec3::new(8.0, 4.0, 3.0));
        // A partition at x = 4, a face to each side.
        let (a, b, c, d) = (
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::new(4.0, 4.0, 0.0),
            Vec3::new(4.0, 4.0, 3.0),
            Vec3::new(4.0, 0.0, 3.0),
        );
        faces.extend(quad(a, b, c, d));
        faces.extend(quad(a, d, c, b));
        let baked = baked(&faces, &[], 0.5);
        // Which side a disc is on; a disc of the partition is on the side
        // it faces.
        let side = |disc: &Disc| disc.pos[0] + disc.normal[0] * 0.1 < 4.0;
        for patch in &baked.patches {
            let run = &baked.discs[patch.start as usize..(patch.start + patch.count) as usize];
            assert!(
                run.iter().all(|d| side(d) == side(&run[0])),
                "a patch straddles the wall"
            );
        }
        for (index, disc) in baked.discs.iter().enumerate() {
            for link in &baked.links[baked.starts[index] as usize..baked.starts[index + 1] as usize]
            {
                // A group may hold both sides; its weight is then from the
                // near side alone, which the total below checks.
                if (link.node as usize) < baked.discs.len() + baked.patches.len() {
                    let from = &baked.discs[members(&baked, link.node).start];
                    assert_eq!(side(from), side(disc), "light through the wall");
                }
            }
        }
        // Each half is a closed room of its own.
        let totals = totals(&baked);
        let mean = totals.iter().sum::<f32>() / totals.len() as f32;
        assert!((mean - 1.0).abs() < 0.06, "a disc takes {mean}");
    }

    /// A raised flag stops the bake.
    #[test]
    fn a_bake_can_be_stopped() {
        let faces = room(Vec3::ZERO, Vec3::splat(4.0));
        let stopped = bake(
            &faces,
            &mesh(&faces),
            &[],
            Settings {
                spacing: 0.5,
                error: 0.01,
            },
            &AtomicBool::new(true),
        );
        assert!(stopped.is_none());
    }
}
