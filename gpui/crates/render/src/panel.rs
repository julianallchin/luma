//! A face of cells as one light: the rectangle an LED panel, a bar or a
//! strobe lights the room from, with its cells as its texture.
//!
//! Every cell of a face points the same way through the same optic. A point
//! `h` metres in front of the face is lit by the cells its beam reaches: with
//! `t` the tangent of the angle off the face's normal, a cell `h·t` across
//! the face from the point sends it `profile(t) / (h²·(1 + t²))` of its
//! light. Summed over the face that is the cells' light per square metre
//! under a kernel whose shape is the beam's and whose size is `h`. Nothing in
//! it is a cone: the light has the shape of whichever cells are lit, and at
//! the face itself it is the face's own brightness, with no clamp on how
//! close.
//!
//! It is taken in two parts. The lit cells' rectangle, evenly lit, under the
//! beam's true kernel: that is exact, from a table of the kernel's integral
//! ([`Footprint`]), and it gives the light its falloff and its edge. Then the
//! pattern inside the rectangle: how much brighter or darker than even the
//! cells are round the point, which is the texture against the same
//! rectangle evenly lit, both under one Gaussian blur as wide as the beam is
//! there. So a face lit all over in one colour is exact at every distance, a
//! lone cell is exact, and what lies between has the right outline and a
//! Gaussian's softness inside it.
//!
//! The pattern is read from a stack made in advance: the texture over the
//! even rectangle, then the same under Gaussian blurs, each half again as
//! wide as the last and kept on a grid no finer than it needs. A point reads
//! the two levels whose widths bracket its own.
//!
//! Far off, where the lit rectangle is a speck in the table, the light is
//! taken in closed form instead: the light of each quarter of the lit cells,
//! from its own centre of light, through the beam's profile widened by that
//! quarter's spread. Far off that is exactly one cone.
//!
//! Haze scatters most of its light nearly straight on, so what a ray picks
//! up at a point depends on where on the face the light there came from, and
//! the lit cells may be in several places. The lit rectangle is cut into
//! [`PARTS`] by [`PARTS`] parts, each the bounds of the cells lit in it, and
//! the light at the point is shared among the parts as each sends it: a
//! point of light through the beam's profile far off, and close to no
//! brighter than a face lit as evenly as the part is. The haze's phase is
//! taken toward each part and weighed by its share ([`Panel::scatter`]). Far
//! off, each quarter is its own cone and has its own direction. A part's
//! light is still taken to come from one place in it, so within a part's own
//! width of a lit part the haze right before it is too bright seen head on
//! and too dim seen from the side.
//!
//! A face of few cells (a bar of lamps, a two-cell strobe: [`FEW`] or fewer)
//! has no texture. Close to, its light is its lit cells' own, summed, each a
//! part of its own: what one cone a cell would send, and scattered from where
//! each cell is. Far off it is the same quarters, so most of a room is lit by
//! the face as two cones or four and not by every cell of it; but haze that
//! is looked through toward the face stays with the cells until they are a
//! speck to it, because the haze's forward peak tells them apart long after
//! their light has run together.
//!
//! The cone the panel names ([`Panel::cone`]) is its bounds, its shadow and
//! its far-field light for the passes that do not read the texture (the glow
//! and the bounce). `shaders/panel.wgsl` reads the buffer; [`Panel::light`]
//! and [`Panel::scatter`] are the same arithmetic, for the tests.

use std::sync::Arc;

use glam::{Vec2, Vec3, Vec4, Vec4Swizzles};
use luma_scene::luminaire::angular_profile;

use crate::frame::FixtureCone;

/// The most texels along a side of a panel's texture.
const MAX_SIDE: u32 = 32;
/// Texels of margin on each side of a blurred level: three of its own
/// widths, past which a Gaussian has nothing left.
const MARGIN: u32 = 3;

/// Parts along each side of the lit rectangle, for the haze's phase.
const PARTS: usize = 3;
/// The most cells of a face that is lit as its cells summed, with no
/// texture.
const FEW: usize = 16;

/// One lit cell of a face.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    /// Its middle on the face, right and up of the panel's cone, metres.
    pub at: Vec2,
    /// Its extent along right and up, metres.
    pub size: Vec2,
    /// Its colour.
    pub colour: Vec3,
    /// How brightly it is lit.
    pub intensity: f32,
}

/// Cells along each side of a footprint's table.
const KERNEL_SIDE: usize = 32;

/// What a beam does to the light of a face a metre behind a plane.
///
/// The kernel is the beam's profile over the plane, `profile(t) / (1 + t²)`
/// at the tangent `t`: its peak is one and it ends at the field's edge. An
/// evenly lit rectangle of the face sends a point the kernel's integral over
/// the rectangle as the point sees it, so the table holds the integral from
/// the axis out to each corner `(u, v)` of a quadrant, and any rectangle is
/// four reads of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Footprint {
    /// The kernel's weight: its integral over the whole plane. A face evenly
    /// lit at one unit per square metre lights a point close in front of it
    /// with this much.
    pub weight: f32,
    /// A metre off, the standard deviation of the Gaussian of that weight
    /// and the kernel's peak, metres: how wide the blur is.
    pub width: f32,
    /// The field's edge as an angle off the beam, radians. The table's rows
    /// and columns are even steps of angle out to it.
    pub edge: f32,
    /// The kernel's integral over `[0, u] × [0, v]` at the four corners of
    /// each cell of the table, row by row: at its low `u` and low `v`, high
    /// `u` and low `v`, low `u` and high `v`, and both high.
    pub kernel: Arc<[Vec4]>,
}

impl Footprint {
    /// The footprint of a beam with these edges.
    #[must_use]
    pub fn of(cos_beam: f32, cos_field: f32) -> Self {
        let edge = cos_field.clamp(0.0, 1.0).acos().clamp(1e-3, 1.55);
        let step = edge / KERNEL_SIDE as f32;
        // The kernel over one cell of the table, at four points of it. In
        // the angles `a` and `b`, `u = tan a` and a patch is `du·dv = da·db /
        // (cos² a · cos² b)`.
        let over = |i: usize, j: usize| {
            let mut sum = 0.0;
            for (di, dj) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let (a, b) = ((i as f32 + di) * step, (j as f32 + dj) * step);
                let (u, v) = (a.tan(), b.tan());
                let cos2 = 1.0 / (1.0 + u * u + v * v);
                sum += angular_profile(cos2.sqrt(), cos_beam, cos_field) * cos2
                    / (a.cos() * a.cos() * b.cos() * b.cos());
            }
            sum * step * step / 4.0
        };
        // The integral out to each node of the table.
        let nodes = KERNEL_SIDE + 1;
        let mut to = vec![0.0_f32; nodes * nodes];
        for j in 0..KERNEL_SIDE {
            for i in 0..KERNEL_SIDE {
                to[(j + 1) * nodes + i + 1] =
                    over(i, j) + to[j * nodes + i + 1] + to[(j + 1) * nodes + i]
                        - to[j * nodes + i];
            }
        }
        let kernel = (0..KERNEL_SIDE * KERNEL_SIDE)
            .map(|cell| {
                let at = cell / KERNEL_SIDE * nodes + cell % KERNEL_SIDE;
                Vec4::new(to[at], to[at + 1], to[at + nodes], to[at + nodes + 1])
            })
            .collect();
        let weight = 4.0 * to[nodes * nodes - 1];
        Self {
            weight,
            width: (weight / std::f32::consts::TAU).sqrt(),
            edge,
            kernel,
        }
    }

    /// `panel_kernel` in `shaders/panel.wgsl`: the kernel's integral over
    /// the rectangle from the axis to `(u, v)`, negative where that crosses
    /// one axis.
    fn quadrant(&self, u: f32, v: f32) -> f32 {
        let p = (Vec2::new(u.abs().atan(), v.abs().atan()) / self.edge).min(Vec2::ONE)
            * KERNEL_SIDE as f32;
        let cell = p.floor().min(Vec2::splat(KERNEL_SIDE as f32 - 1.0));
        let f = p - cell;
        let c = self.kernel[cell.y as usize * KERNEL_SIDE + cell.x as usize];
        let sign = |x: f32| {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        };
        sign(u) * sign(v) * lerp(lerp(c.x, c.y, f.x), lerp(c.z, c.w, f.x), f.y)
    }

    /// The kernel's integral over the rectangle from `lo` to `hi`, tangents
    /// as a point sees it: what the rectangle sends the point for each unit
    /// of light per square metre on it.
    #[must_use]
    pub fn rectangle(&self, lo: Vec2, hi: Vec2) -> f32 {
        self.quadrant(hi.x, hi.y) - self.quadrant(lo.x, hi.y) - self.quadrant(hi.x, lo.y)
            + self.quadrant(lo.x, lo.y)
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The light a panel sends to a point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Light {
    /// What `colour · profile / distance²` is for a cone.
    pub colour: Vec3,
    /// The same for a white face: how much light, whatever its colour.
    pub white: f32,
    /// Where on the face it comes from, from the cone's position.
    pub source: Vec3,
}

/// A face of cells behind one cone.
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    /// Its cone's index in [`crate::Frame::fixture_cones`].
    pub cone: usize,
    /// The face's right, unit, world space. Its up is the cone's direction
    /// crossed with this.
    pub right: Vec3,
    /// The texture's middle, right and up of the cone's position, metres.
    pub middle: Vec2,
    /// One texel's side, metres; the smallest cell's, for a face of few
    /// cells.
    pub texel: f32,
    /// Texels along each side of the texture: a power of two, or zero for a
    /// face of few cells, which has no texture.
    pub side: u32,
    /// The pattern the cells make inside their lit rectangle: their light
    /// against the rectangle's evenly lit, one all over an even face, `xyz`
    /// in the cells' colours over the cone's and `w` in white. The texture
    /// comes first, row by row from the bottom left, then the same under
    /// each blur ([`stack`]).
    pub stack: Vec<Vec4>,
    /// The lit cells' centre of light, right and up of the cone's position
    /// (`xy`), and their variance about it along each (`zw`, square metres).
    pub lit: Vec4,
    /// The same for the lit cells of each quarter of their extent (left and
    /// right of the cone, below and above it), and each quarter's share of
    /// the light: `xyz` in its colour, `w` in white. Far off, the face is
    /// these four cones, so two halves of two colours keep their own.
    pub quarters: [(Vec4, Vec4); 4],
    /// The lit cells in parts: the bounds of each part's cells (`xy` its low
    /// corner, `zw` its high, right and up of the cone's position), and their
    /// share of the light (`xyz` in colour, `w` in white). A face of few
    /// cells has a part a lit cell; any other has the lit rectangle cut
    /// [`PARTS`] by [`PARTS`], less the parts with no lit cell.
    pub parts: Vec<(Vec4, Vec4)>,
    /// Half the lit cells' extent along right and up, metres: the cone sits
    /// at their middle.
    pub reach: Vec2,
    /// The beam every cell casts.
    pub footprint: Footprint,
}

impl Panel {
    /// The panel of a face of `face_cells` cells that spans `lo` to `hi`
    /// (right and up of the cone's position, metres), lit by `cells`, whose
    /// smallest side is `finest` metres. The texture covers the whole face
    /// whichever cells are lit, so a chase crosses a grid that stays still,
    /// and it is the face's cells and not the lit ones that say whether it
    /// has a texture at all.
    #[must_use]
    pub fn new(
        cone: usize,
        right: Vec3,
        (lo, hi): (Vec2, Vec2),
        finest: f32,
        (cells, face_cells): (&[Cell], usize),
        footprint: Footprint,
    ) -> Self {
        let few = face_cells <= FEW;
        let middle = (lo + hi) / 2.0;
        let extent = (hi - lo).max_element().max(1e-3);
        // Two texels across the smallest cell, where the texture may be
        // that fine.
        let side = if few {
            0
        } else {
            ((2.0 * extent / finest.max(1e-4)).ceil() as u32)
                .next_power_of_two()
                .clamp(2, MAX_SIDE)
        };
        let texel = if few { finest } else { extent / side as f32 };
        let total: f32 = cells.iter().map(|cell| cell.intensity).sum();
        let total = total.max(1e-12);
        let corner = middle - Vec2::splat(extent / 2.0);
        let mut texels = vec![Vec4::ZERO; (side * side) as usize];
        for cell in cells.iter().filter(|_| !few) {
            // Each texel takes the part of the cell that lies in it.
            let size = cell.size.max(Vec2::splat(1e-4));
            let from = (cell.at - size / 2.0 - corner) / texel;
            let to = (cell.at + size / 2.0 - corner) / texel;
            let light = cell.colour.extend(1.0) * (cell.intensity / total / (size.x * size.y));
            let last = side as i32 - 1;
            for y in (from.y.floor() as i32).max(0)..=(to.y.ceil() as i32 - 1).min(last) {
                for x in (from.x.floor() as i32).max(0)..=(to.x.ceil() as i32 - 1).min(last) {
                    let at = Vec2::new(x as f32, y as f32);
                    let covered = (to.min(at + 1.0) - from.max(at)).max(Vec2::ZERO);
                    texels[(y * side as i32 + x) as usize] += light * (covered.x * covered.y);
                }
            }
        }
        // The lit cells' rectangle, evenly lit with the same light.
        let reach = cells
            .iter()
            .map(|cell| cell.at.abs() + cell.size / 2.0)
            .fold(Vec2::ZERO, Vec2::max);
        let mut even = vec![Vec4::ZERO; (side * side) as usize];
        let (from, to) = ((-reach - corner) / texel, (reach - corner) / texel);
        for y in 0..side {
            for x in 0..side {
                let at = Vec2::new(x as f32, y as f32);
                let covered = (to.min(at + 1.0) - from.max(at)).max(Vec2::ZERO);
                even[(y * side + x) as usize] =
                    Vec4::splat(covered.x * covered.y / (4.0 * reach.x * reach.y));
            }
        }
        // A set of cells' centre of light and spread, and its light.
        let moments = |among: &dyn Fn(&Cell) -> bool| {
            let cells = || cells.iter().filter(|cell| among(cell));
            let sum: f32 = cells().map(|cell| cell.intensity).sum();
            if sum <= 0.0 {
                return (Vec4::new(0.0, 0.0, 1e-8, 1e-8), Vec4::ZERO);
            }
            let centre = cells().map(|cell| cell.at * cell.intensity).sum::<Vec2>() / sum;
            let spread = cells()
                .map(|cell| {
                    let off = cell.at - centre;
                    (off * off + cell.size * cell.size / 12.0) * cell.intensity
                })
                .sum::<Vec2>()
                / sum;
            let light = cells()
                .map(|cell| cell.colour.extend(1.0) * cell.intensity)
                .sum::<Vec4>()
                / total;
            (
                centre.extend(spread.x.max(1e-8)).extend(spread.y.max(1e-8)),
                light,
            )
        };
        let quarter = |right: bool, up: bool| {
            moments(&|cell| (cell.at.x >= 0.0) == right && (cell.at.y >= 0.0) == up)
        };
        // The lit cells in parts: each its own on a face of few, and else
        // those in each part of the lit rectangle, by their middles.
        let mut parts =
            vec![(Vec4::ZERO, Vec4::ZERO); if few { cells.len() } else { PARTS * PARTS }];
        let mut bounds = vec![(Vec2::INFINITY, Vec2::NEG_INFINITY); parts.len()];
        for (index, cell) in cells.iter().enumerate() {
            let part = if few {
                index
            } else {
                let place = ((cell.at + reach) / (2.0 * reach).max(Vec2::splat(1e-6))
                    * PARTS as f32)
                    .floor()
                    .clamp(Vec2::ZERO, Vec2::splat(PARTS as f32 - 1.0));
                place.y as usize * PARTS + place.x as usize
            };
            let (lo, hi) = &mut bounds[part];
            *lo = lo.min(cell.at - cell.size / 2.0);
            *hi = hi.max(cell.at + cell.size / 2.0);
            parts[part].1 += cell.colour.extend(1.0) * (cell.intensity / total);
        }
        for ((lo, hi), part) in bounds.into_iter().zip(&mut parts) {
            if part.1.w > 0.0 {
                part.0 = lo.extend(hi.x).extend(hi.y);
            }
        }
        parts.retain(|(_, light)| light.w > 0.0);
        Self {
            cone,
            right,
            middle,
            texel,
            side,
            stack: if few {
                Vec::new()
            } else {
                pattern(
                    &stack(texels, side as usize),
                    &stack(even, side as usize),
                    side as usize,
                )
            },
            lit: moments(&|_| true).0,
            quarters: [
                quarter(false, false),
                quarter(true, false),
                quarter(false, true),
                quarter(true, true),
            ],
            parts,
            reach,
            footprint,
        }
    }

    /// Pairs of blurred levels in the stack: enough for a blur four times
    /// the texture's width.
    fn pairs(&self) -> u32 {
        self.side.trailing_zeros() + 3
    }

    /// `panel_level` in `shaders/panel.wgsl`: the stack's `level` at `at`
    /// metres from the texture's middle, between its samples.
    fn level(&self, level: u32, at: Vec2) -> Vec4 {
        let side = self.side as i32;
        let (mut table, mut n, mut grid) = (0, side, self.texel);
        if level > 0 {
            table = side * side;
            n = side + 2 * MARGIN as i32;
            for pair in 0..(level - 1) / 2 {
                table += 2 * n * n;
                n = (side >> (pair + 1)).max(2) + 2 * MARGIN as i32;
                grid *= 2.0;
            }
            if (level - 1) % 2 == 1 {
                table += n * n;
            }
        }
        let p = at / grid + 0.5 * (n - 1) as f32;
        let cell = p.floor();
        let f = p - cell;
        // Past a level's edge the pattern is as it was at the edge.
        let read = |dx: i32, dy: i32| {
            let (x, y) = (
                (cell.x as i32 + dx).clamp(0, n - 1),
                (cell.y as i32 + dy).clamp(0, n - 1),
            );
            self.stack[(table + y * n + x) as usize]
        };
        read(0, 0)
            .lerp(read(1, 0), f.x)
            .lerp(read(0, 1).lerp(read(1, 1), f.x), f.y)
    }

    /// The light `cone` sends to the point `q` metres from its position:
    /// `panel_light` in `shaders/panel.wgsl`.
    #[must_use]
    pub fn light(&self, cone: &FixtureCone, q: Vec3) -> Light {
        self.lit(cone, q, None)
    }

    /// The light haze at `q` scatters along `ray`, a unit vector, where
    /// `phase` is the haze's phase function of the cosine between the way
    /// light arrives from and the ray: `panel_scatter` in
    /// `shaders/panel.wgsl`. Each part of the lit cells is scattered from
    /// where it is.
    #[must_use]
    pub fn scatter(
        &self,
        cone: &FixtureCone,
        q: Vec3,
        ray: Vec3,
        phase: &dyn Fn(f32) -> f32,
    ) -> Light {
        self.lit(cone, q, Some((ray, phase)))
    }

    /// `panel_parts` in `shaders/panel.wgsl`: what the parts of the lit
    /// cells send the point between them, and the same with each part's
    /// times the haze's phase toward it. `toward` is the phase toward a place
    /// on the face.
    fn parts_send(
        &self,
        cone: &FixtureCone,
        (at, height, blur2): (Vec2, f32, f32),
        toward: &dyn Fn(Vec2) -> f32,
    ) -> (Vec4, Vec4) {
        let (mut light, mut scattered) = (Vec4::ZERO, Vec4::ZERO);
        for (bounds, share) in &self.parts {
            // Close to, the part's light comes from its nearest place to the
            // point; further off, from nearer its middle.
            let (lo, hi) = (bounds.xy(), bounds.zw());
            let (centre, size) = ((lo + hi) / 2.0, hi - lo);
            let spread = size * size / 12.0;
            let seat =
                centre + (at.clamp(lo, hi) - centre) * (spread / (Vec2::splat(blur2) + spread));
            // What the beam passes from there to the point.
            let cos2 = 1.0 / (1.0 + ((seat - at) / height).length_squared());
            let reaches = angular_profile(cos2.sqrt(), cone.cos_beam, cone.cos_field) * cos2;
            if reaches <= 0.0 {
                continue;
            }
            // Far off the part is a point of light there; close to, it is
            // no brighter than a face lit as evenly as it is.
            let sends = *share
                * (reaches / (height * height + size.x * size.y * reaches / self.footprint.weight));
            light += sends;
            scattered += sends * toward(seat);
        }
        (light, scattered)
    }

    /// `panel_lit` in `shaders/panel.wgsl`, term for term: the light at
    /// `q`, and with `scatter` (a ray and the haze's phase) each part of it
    /// times the phase from where it comes toward the ray.
    fn lit(
        &self,
        cone: &FixtureCone,
        q: Vec3,
        scatter: Option<(Vec3, &dyn Fn(f32) -> f32)>,
    ) -> Light {
        let up = cone.direction.cross(self.right);
        // The haze's phase toward a place on the face; one for plain light.
        let toward = |seat: Vec2| {
            scatter.map_or(1.0, |(ray, phase)| {
                phase((self.right * seat.x + up * seat.y - q).normalize().dot(ray))
            })
        };
        let height = q.dot(cone.direction);
        let mut out = Light {
            colour: Vec3::ZERO,
            white: 0.0,
            source: Vec3::ZERO,
        };
        if height <= 0.0 {
            return out;
        }
        let at = Vec2::new(q.dot(self.right), q.dot(up));
        let blur = self.footprint.width * height;
        let blur2 = blur * blur;
        // How much of the light's spread at this distance is the beam's and
        // not the cells' own: the light comes from that much nearer the
        // cells' centre than the point is, and never from outside the lit
        // rectangle.
        let (centre, spread) = (self.lit.xy(), self.lit.zw());
        let share = Vec2::splat(blur2) / (Vec2::splat(blur2) + spread);
        let from = centre + (at.clamp(-self.reach, self.reach) - centre) * (Vec2::ONE - share);
        out.source = self.right * from.x + up * from.y;
        let mut far = smoothstep(2.0, 4.0, blur / spread.max_element().sqrt());
        if let (Some((ray, _)), 0) = (scatter, self.side) {
            // A face of few cells can always be its cells summed. For the
            // haze it is its quarters only where one direction a quarter
            // will do: where the haze's phase barely changes across the lit
            // cells as the point sees them.
            let to = self.right * centre.x + up * centre.y - q;
            let seen = spread.max_element() / to.length_squared();
            far *= 1.0
                - smoothstep(
                    0.02,
                    0.06,
                    crate::haze_phase::spread(to.normalize().dot(ray), seen),
                );
        }
        if far > 0.0 {
            // One cone a quarter, from that quarter's own centre.
            for (lit, light) in self.quarters {
                let share = Vec2::splat(blur2) / (Vec2::splat(blur2) + lit.zw());
                let t = (at - lit.xy()) * Vec2::new(share.x.sqrt(), share.y.sqrt()) / height;
                let cos2 = 1.0 / (1.0 + t.length_squared());
                let seat = lit.xy() + (at - lit.xy()) * (Vec2::ONE - share);
                let one = far
                    * (share.x * share.y).sqrt()
                    * angular_profile(cos2.sqrt(), cone.cos_beam, cone.cos_field)
                    * cos2
                    / (height * height)
                    * toward(seat);
                out.colour += light.truncate() * one;
                out.white += light.w * one;
            }
        }
        if far < 1.0 && self.side == 0 {
            // A face of few cells: its lit cells' own light, summed.
            let (light, scattered) = self.parts_send(cone, (at, height, blur2), &toward);
            let near = if scatter.is_some() { scattered } else { light } * (1.0 - far);
            out.colour += near.truncate();
            out.white += near.w;
        } else if far < 1.0 {
            // The blur and the texture's own grain together, in half texels,
            // squared: one for the texture, and doubling a level. The two
            // levels that bracket this blur are mixed to give a lone cell
            // the brightness it leaves: brightness goes as one over the
            // square.
            let reach2 = 4.0 * blur2 / (self.texel * self.texel) + 1.0;
            let level = (reach2.log2().floor().max(0.0) as u32).min(2 * self.pairs() - 1);
            let mixing = (2.0 * (1.0 - (level as f32).exp2() / reach2)).clamp(0.0, 1.0);
            // The pattern, read inside the lit rectangle: past its edge it
            // is as it was at the edge.
            let inside = at.clamp(-self.reach, self.reach) - self.middle;
            let pattern = self
                .level(level, inside)
                .lerp(self.level(level + 1, inside), mixing);
            // The rectangle, evenly lit, under the beam's own kernel.
            let rectangle = self
                .footprint
                .rectangle((-self.reach - at) / height, (self.reach - at) / height)
                / (4.0 * self.reach.x * self.reach.y);
            let mut near = pattern * (rectangle * (1.0 - far));
            if scatter.is_some() {
                // The haze's phase toward each part, weighed by what the
                // part sends; toward the whole where no part sends anything.
                let (light, scattered) = self.parts_send(cone, (at, height, blur2), &toward);
                near *= Vec4::select(
                    light.cmpgt(Vec4::ZERO),
                    scattered / light.max(Vec4::splat(1e-30)),
                    Vec4::splat(toward(from)),
                );
            }
            out.colour += near.truncate();
            out.white += near.w;
        }
        out
    }
}

/// A texture `side` texels a side, then its blurred levels in pairs. Level
/// `k` reads as the texture under a Gaussian `2^(k/2)` half texels wide,
/// counting the blur of reading between samples. A pair shares a grid: the
/// first pair the texture's own, each later pair one twice as coarse, all
/// centred on the texture, with [`MARGIN`] samples of margin a side.
fn stack(texture: Vec<Vec4>, side: usize) -> Vec<Vec4> {
    let margin = MARGIN as usize;
    // Along rows, then columns: `taps` either side of each sample.
    let blur = |image: &[Vec4], n: usize, taps: &[f32]| {
        let mut out = image.to_vec();
        for axis in 0..2 {
            let from = out.clone();
            for y in 0..n {
                for x in 0..n {
                    let mut sum = from[y * n + x] * taps[0];
                    for (step, tap) in taps.iter().enumerate().skip(1) {
                        let (along, stride) = if axis == 0 { (x, 1) } else { (y, n) };
                        if along >= step {
                            sum += from[y * n + x - step * stride] * *tap;
                        }
                        if along + step < n {
                            sum += from[y * n + x + step * stride] * *tap;
                        }
                    }
                    out[y * n + x] = sum;
                }
            }
        }
        out
    };
    // A quarter of a square texel of variance, and a half.
    let (quarter, half) = ([0.75, 0.125], [0.564_21, 0.207_56, 0.010_335]);
    let mut n = side + 2 * margin;
    let mut level = vec![Vec4::ZERO; n * n];
    for y in 0..side {
        for x in 0..side {
            level[(y + margin) * n + x + margin] = texture[y * side + x];
        }
    }
    let mut levels = texture;
    let pairs = side.trailing_zeros() as usize + 3;
    for pair in 0..pairs {
        let mut first = blur(&level, n, &quarter);
        if pair > 0 {
            // Onto a grid twice as coarse: each sample the mean of the four
            // it sits between.
            let coarse = (side >> pair).max(2) + 2 * margin;
            let shift = n as i32 / 2 - coarse as i32;
            let read = |x: i32, y: i32| {
                if x < 0 || y < 0 || x >= n as i32 || y >= n as i32 {
                    Vec4::ZERO
                } else {
                    first[y as usize * n + x as usize]
                }
            };
            let mut onto = vec![Vec4::ZERO; coarse * coarse];
            for y in 0..coarse {
                for x in 0..coarse {
                    let (i, j) = (2 * x as i32 + shift, 2 * y as i32 + shift);
                    onto[y * coarse + x] =
                        (read(i, j) + read(i + 1, j) + read(i, j + 1) + read(i + 1, j + 1)) / 4.0;
                }
            }
            first = onto;
            n = coarse;
        }
        level = blur(&first, n, &half);
        levels.extend_from_slice(&first);
        levels.extend_from_slice(&level);
    }
    levels
}

/// Where each level of a stack starts and how many samples a side it has.
fn levels(side: usize) -> Vec<(usize, usize)> {
    let mut levels = vec![(0, side)];
    let (mut at, mut n) = (side * side, side + 2 * MARGIN as usize);
    for pair in 0..side.trailing_zeros() as usize + 3 {
        levels.push((at, n));
        levels.push((at + n * n, n));
        at += 2 * n * n;
        n = (side >> (pair + 1)).max(2) + 2 * MARGIN as usize;
    }
    levels
}

/// The stack of a texture over the stack of its lit rectangle evenly lit,
/// sample by sample: the pattern, level by level. Where a level has nothing
/// of the rectangle left, the pattern is the nearest sample's that has.
fn pattern(texture: &[Vec4], even: &[Vec4], side: usize) -> Vec<Vec4> {
    let mut out = vec![Vec4::ZERO; texture.len()];
    for (at, n) in levels(side) {
        let even = |x: usize, y: usize| even[at + y * n + x].x;
        let most = (0..n * n).map(|i| even(i % n, i / n)).fold(0.0, f32::max);
        let has = |x: usize, y: usize| even(x, y) > 1e-3 * most;
        // An even rectangle under a blur is a product of its two sides, so
        // the samples with any of it left are a rectangle too.
        let held = |along: &dyn Fn(usize) -> bool| {
            let first = (0..n).find(|&i| along(i)).unwrap_or(0);
            let last = (0..n).rfind(|&i| along(i)).unwrap_or(n - 1);
            (first, last)
        };
        let (left, right) = held(&|x| (0..n).any(|y| has(x, y)));
        let (bottom, top) = held(&|y| (0..n).any(|x| has(x, y)));
        for y in 0..n {
            for x in 0..n {
                let (i, j) = (x.clamp(left, right), y.clamp(bottom, top));
                out[at + y * n + x] = texture[at + j * n + i] / even(i, j).max(1e-30);
            }
        }
    }
    out
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Every panel's header, parts and stack in one buffer, and where each drawn
/// cone's panel starts in it. `kept` is each drawn cone's index in the frame,
/// which is what a panel names its cone by. A cone with no panel starts at
/// zero, which is why the buffer opens with a word nothing reads.
pub(crate) fn pack(panels: &[Panel], kept: &[usize]) -> (Vec<[f32; 4]>, Vec<f32>) {
    let mut words = vec![[0.0; 4]];
    let starts = kept
        .iter()
        .map(|cone| {
            let Some(panel) = panels.iter().find(|panel| panel.cone == *cone) else {
                return 0.0;
            };
            let start = words.len();
            words.push(panel.right.extend(panel.texel).to_array());
            words.push([
                panel.middle.x,
                panel.middle.y,
                panel.side as f32,
                panel.footprint.edge,
            ]);
            words.push([
                panel.footprint.weight,
                panel.footprint.width,
                panel.reach.x,
                panel.reach.y,
            ]);
            words.push(panel.lit.to_array());
            words.extend(panel.quarters.iter().map(|(lit, _)| lit.to_array()));
            words.extend(panel.quarters.iter().map(|(_, light)| light.to_array()));
            words.push([panel.parts.len() as f32, 0.0, 0.0, 0.0]);
            for (bounds, light) in &panel.parts {
                words.extend([bounds.to_array(), light.to_array()]);
            }
            // A face of few cells has no texture, and reads no table.
            if panel.side > 0 {
                words.extend(panel.footprint.kernel.iter().map(Vec4::to_array));
                words.extend(panel.stack.iter().map(Vec4::to_array));
            }
            start as f32
        })
        .collect();
    (words, starts)
}

#[cfg(test)]
mod tests {
    use super::{Cell, Footprint, Panel, KERNEL_SIDE};
    use crate::frame::FixtureCone;
    use glam::{Vec2, Vec3};
    use luma_scene::luminaire::{angular_profile, Lens};

    /// A cone facing down `-Z` from the origin: a 20 degree beam inside a 36
    /// degree field.
    fn cone(color: Vec3) -> FixtureCone {
        FixtureCone {
            position: Vec3::ZERO,
            range: 30.0,
            direction: Vec3::NEG_Z,
            cos_beam: 10.0_f32.to_radians().cos(),
            color,
            intensity: 1.0,
            cos_field: 18.0_f32.to_radians().cos(),
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
            lens: Lens { radius: 0.2 },
        }
    }

    const PITCH: f32 = 0.04;

    /// Eight cells by four, 4 cm apart with a 3 cm face each; `lit` gives
    /// each column and row's colour, black for a dark cell.
    fn panel(lit: impl Fn(usize, usize) -> Vec3) -> (Panel, FixtureCone) {
        let mut cells = Vec::new();
        for row in 0..4 {
            for column in 0..8 {
                let colour = lit(column, row);
                if colour.max_element() > 0.0 {
                    cells.push(Cell {
                        at: Vec2::new(column as f32 - 3.5, row as f32 - 1.5) * PITCH,
                        size: Vec2::splat(0.03),
                        colour,
                        intensity: 1.0,
                    });
                }
            }
        }
        let mean = cells.iter().map(|cell| cell.colour).sum::<Vec3>() / cells.len() as f32;
        let cone = cone(mean);
        let half = Vec2::new(4.0, 2.0) * PITCH;
        let panel = Panel::new(
            0,
            Vec3::X,
            (-half, half),
            0.03,
            (&cells, 32),
            Footprint::of(cone.cos_beam, cone.cos_field),
        );
        (panel, cone)
    }

    /// The point `at` across the face and `height` in front of it. The cone
    /// faces down -Z with its right along X, so its up is -Y.
    fn point(at: Vec2, height: f32) -> Vec3 {
        Vec3::new(at.x, -at.y, -height)
    }

    /// All the white light crossing the plane `height` in front of the face.
    fn crossing(panel: &Panel, cone: &FixtureCone, height: f32) -> f32 {
        let reach = 0.2 + height * 0.4;
        let steps = 240;
        let cell = 2.0 * reach / steps as f32;
        let mut sum = 0.0;
        for y in 0..steps {
            for x in 0..steps {
                let at = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) * cell - reach;
                sum += panel.light(cone, point(at, height)).white;
            }
        }
        sum * cell * cell
    }

    /// The light crossing a plane in front of the face is the beam's own
    /// weight, the light a face of one unit sends, however far off the plane
    /// is: touching the face, a hand's width off, across the room.
    #[test]
    fn the_same_light_crosses_every_plane_before_the_face() {
        type Lit<'a> = &'a dyn Fn(usize, usize) -> Vec3;
        let patterns: [(&str, f32, Lit); 3] = [
            ("whole", 0.95, &|_, _| Vec3::ONE),
            ("corner", 0.95, &|column, row| {
                if column == 0 && row == 0 {
                    Vec3::ONE
                } else {
                    Vec3::ZERO
                }
            }),
            ("checks", 0.95, &|column, row| {
                if (column + row) % 2 == 0 {
                    Vec3::ONE
                } else {
                    Vec3::ZERO
                }
            }),
        ];
        for (name, least, lit) in patterns {
            let (panel, cone) = panel(lit);
            for height in [0.002, 0.03, 0.1, 0.3, 1.0, 4.0] {
                let light = crossing(&panel, &cone, height) / panel.footprint.weight;
                assert!(
                    (least..1.05).contains(&light),
                    "{name} at {height} m: {light} of the beam's light"
                );
            }
        }
    }

    /// Touching the face a point is lit by the cell it touches, in that
    /// cell's colour, and before a dark cell it is dark.
    #[test]
    fn a_point_at_the_face_is_lit_by_its_own_cell() {
        let (red, blue) = (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
        let (panel, cone) = panel(|column, row| match (column, row) {
            (_, 0) => Vec3::ZERO,
            (0..=3, _) => red,
            _ => blue,
        });
        let over = |column: f32, row: f32| {
            panel
                .light(
                    &cone,
                    point(Vec2::new(column - 3.5, row - 1.5) * PITCH, 0.001),
                )
                .colour
        };
        let (at_red, at_blue, at_dark) = (over(1.0, 2.0), over(6.0, 2.0), over(1.0, 0.0));
        assert!(at_red.x > 0.0 && at_red.z < 0.01 * at_red.x, "{at_red}");
        assert!(at_blue.z > 0.0 && at_blue.x < 0.01 * at_blue.z, "{at_blue}");
        assert!(at_dark.max_element() < 0.01 * at_red.x, "{at_dark}");
        // Between two cells of one colour it is the gap, darker than a cell.
        let gap = over(1.5, 2.0);
        assert!(gap.x < 0.6 * at_red.x, "gap {gap}, cell {at_red}");
    }

    /// Far off, the face is one cone at its lit cells' centre: half a face
    /// lit lights the room from the middle of that half.
    #[test]
    fn far_off_the_lit_cells_are_one_cone_at_their_centre() {
        let (panel, cone) = panel(|column, _| if column < 4 { Vec3::ONE } else { Vec3::ZERO });
        let centre = panel.lit.truncate().truncate();
        assert!((centre.x + 2.0 * PITCH).abs() < 1e-5 && centre.y.abs() < 1e-5);
        for at in [Vec2::ZERO, Vec2::new(0.5, 0.2), Vec2::new(-1.0, 0.6)] {
            let q = point(at, 6.0);
            let from = q - point(centre, 0.0);
            let cos = from.normalize().dot(cone.direction);
            let one = angular_profile(cos, cone.cos_beam, cone.cos_field) / from.length_squared();
            let light = panel.light(&cone, q).white;
            assert!(
                (light / one - 1.0).abs() < 0.02,
                "{at}: {light} against {one}"
            );
        }
    }

    /// Backing away from a lit cell the light never steps: not where the
    /// stack changes level, and not where the closed form takes over.
    #[test]
    fn the_light_changes_without_a_step_backing_away() {
        let (panel, cone) = panel(|column, row| {
            if (column + row) % 3 == 0 {
                Vec3::ONE
            } else {
                Vec3::ZERO
            }
        });
        for at in [Vec2::new(-3.5, -1.5) * PITCH, Vec2::new(0.01, 0.03)] {
            let mut height = 0.001_f32;
            let mut previous = panel.light(&cone, point(at, height)).white;
            // A fifth of a percent further each time: the light falls a few
            // times as fast as the distance grows, and a step would be more.
            while height < 8.0 {
                height *= 1.002;
                let light = panel.light(&cone, point(at, height)).white;
                assert!(
                    (light - previous).abs() <= 0.02 * light.max(previous),
                    "a step at {height} m over {at}: {previous} to {light}"
                );
                previous = light;
            }
        }
    }

    /// Henyey-Greenstein's forward peak, about what haze droplets have.
    fn forward(cos: f32) -> f32 {
        let g = 0.855_f32;
        (1.0 - g * g) / (1.0 + g * g - 2.0 * g * cos).powf(1.5)
    }

    /// What haze at `q` scatters along `ray` from each cell's own beam.
    fn cells_scatter(cells: &[Cell], cone: &FixtureCone, q: Vec3, ray: Vec3) -> f32 {
        // Each cell as a grid of small sources across its face.
        let fine = 12;
        cells
            .iter()
            .flat_map(|cell| {
                (0..fine * fine).map(move |k| {
                    let across = Vec2::new((k % fine) as f32 + 0.5, (k / fine) as f32 + 0.5)
                        / fine as f32
                        - 0.5;
                    cell.at + across * cell.size
                })
            })
            .map(|at| {
                let from = q - point(at, 0.0);
                let cos = from.normalize().dot(cone.direction);
                angular_profile(cos, cone.cos_beam, cone.cos_field) / from.length_squared()
                    * forward(-from.normalize().dot(ray))
            })
            .sum::<f32>()
            / (cells.len() * fine * fine) as f32
    }

    /// A wide beam over two blocks of cells at the ends of the face, with
    /// the middle four columns dark.
    fn two_blocks() -> (Panel, FixtureCone, Vec<Cell>) {
        let mut cone = cone(Vec3::ONE);
        cone.cos_beam = 50.0_f32.to_radians().cos();
        cone.cos_field = 70.0_f32.to_radians().cos();
        let cells: Vec<Cell> = (0..32)
            .filter(|k| !(2..6).contains(&(k % 8)))
            .map(|k| Cell {
                at: Vec2::new((k % 8) as f32 - 3.5, (k / 8) as f32 - 1.5) * PITCH,
                size: Vec2::splat(PITCH),
                colour: Vec3::ONE,
                intensity: 1.0,
            })
            .collect();
        let half = Vec2::new(4.0, 2.0) * PITCH;
        let footprint = Footprint::of(cone.cos_beam, cone.cos_field);
        let panel = Panel::new(0, Vec3::X, (-half, half), PITCH, (&cells, 32), footprint);
        (panel, cone, cells)
    }

    /// Haze that scattered evenly every way would pick up the light itself:
    /// taking the light a part at a time loses none and adds none.
    #[test]
    fn haze_that_scatters_evenly_picks_up_the_light_itself() {
        let (panel, cone, _) = two_blocks();
        for height in [0.01_f32, 0.05, 0.2, 0.6, 2.0, 8.0] {
            for across in [Vec2::ZERO, Vec2::new(0.13, 0.02), Vec2::new(0.3, -0.1)] {
                let q = point(across, height);
                let (light, scattered) = (
                    panel.light(&cone, q),
                    panel.scatter(&cone, q, Vec3::Y, &|_| 1.0),
                );
                assert!(
                    (scattered.white - light.white).abs() <= 1e-4 * light.white,
                    "{height} m off, {across} across: {} against {}",
                    scattered.white,
                    light.white
                );
            }
        }
    }

    /// Looking straight into the face between two lit blocks, the haze
    /// there is lit from the blocks to either side, not from the dark cells
    /// behind it: it scatters what the cells' own beams would, far less
    /// than light from straight behind.
    #[test]
    fn haze_between_two_lit_blocks_is_lit_from_the_blocks() {
        let (panel, cone, cells) = two_blocks();
        let ray = -cone.direction;
        for height in [0.1_f32, 0.2, 0.5, 1.0, 3.0] {
            for across in [0.0_f32, 0.02] {
                let q = point(Vec2::new(across, 0.01), height);
                let (panel, cells) = (
                    panel.scatter(&cone, q, ray, &forward).white,
                    cells_scatter(&cells, &cone, q, ray),
                );
                assert!(
                    (panel / cells - 1.0).abs() < 0.15,
                    "{height} m off, {across} m across: {panel} against {cells}"
                );
            }
        }
        // A hand's width off, light from straight behind would scatter
        // many times as much.
        let q = point(Vec2::ZERO, 0.1);
        let behind = panel.light(&cone, q).white * forward(1.0);
        assert!(panel.scatter(&cone, q, ray, &forward).white < 0.1 * behind);
    }

    /// A bar of ten lamps a decimetre apart under a wide beam, with the
    /// lamps `lit` gives.
    fn bar(lit: impl Fn(usize) -> bool) -> (Panel, FixtureCone, Vec<Cell>) {
        let mut cone = cone(Vec3::ONE);
        cone.cos_beam = 50.0_f32.to_radians().cos();
        cone.cos_field = 70.0_f32.to_radians().cos();
        let cells: Vec<Cell> = (0..10)
            .filter(|lamp| lit(*lamp))
            .map(|lamp| Cell {
                at: Vec2::new((lamp as f32 - 4.5) * 0.1, 0.0),
                size: Vec2::splat(0.04),
                colour: Vec3::ONE,
                intensity: 1.0,
            })
            .collect();
        let half = Vec2::new(0.5, 0.04);
        let footprint = Footprint::of(cone.cos_beam, cone.cos_field);
        let panel = Panel::new(0, Vec3::X, (-half, half), 0.04, (&cells, 10), footprint);
        (panel, cone, cells)
    }

    /// What the cells send `q`, a cone a cell from the cell's middle, and
    /// what haze there scatters of it along `ray`.
    fn cones(cells: &[Cell], cone: &FixtureCone, q: Vec3, ray: Vec3) -> (f32, f32) {
        let (mut light, mut scattered) = (0.0, 0.0);
        for cell in cells {
            let from = q - point(cell.at, 0.0);
            let cos = from.normalize().dot(cone.direction);
            let one = angular_profile(cos, cone.cos_beam, cone.cos_field) / from.length_squared();
            light += one;
            scattered += one * forward(-from.normalize().dot(ray));
        }
        (light / cells.len() as f32, scattered / cells.len() as f32)
    }

    /// A face of few cells has no texture: from a hand's width off to where
    /// it is one cone, it lights a point as a cone a cell would, and the
    /// haze scatters it as from each cell's own place. Its buffer is its
    /// header and its lit cells.
    #[test]
    fn a_face_of_few_cells_is_lit_as_a_cone_a_cell() {
        type Lit<'a> = &'a dyn Fn(usize) -> bool;
        let patterns: [(&str, Lit); 3] = [
            ("all", &|_| true),
            ("ends", &|lamp| !(2..8).contains(&lamp)),
            ("one", &|lamp| lamp == 7),
        ];
        for (name, lit) in patterns {
            let (panel, cone, cells) = bar(lit);
            assert_eq!((panel.side, panel.stack.len()), (0, 0));
            let (words, _) = super::pack(std::slice::from_ref(&panel), &[0]);
            assert_eq!(words.len(), 1 + 13 + 2 * cells.len());
            // Looking into the face, across it, and away from it.
            let rays = [
                -cone.direction,
                Vec3::Y,
                (cone.direction + Vec3::X).normalize(),
            ];
            for height in [0.12_f32, 0.3, 0.7, 1.5, 4.0, 12.0] {
                for across in [Vec2::ZERO, Vec2::new(0.23, 0.05), Vec2::new(-0.6, 0.3)] {
                    let q = point(across * (1.0 + height), height);
                    let floor = 1e-3 / (height * height);
                    let light = panel.light(&cone, q).white;
                    for ray in rays {
                        let (sum, sum_scattered) = cones(&cells, &cone, q, ray);
                        assert!(
                            (light - sum).abs() < 0.04 * sum.max(floor),
                            "{name}, {height} m off, {across}: {light} against {sum}"
                        );
                        let scattered = panel.scatter(&cone, q, ray, &forward).white;
                        assert!(
                            (scattered - sum_scattered).abs() < 0.06 * sum_scattered.max(floor),
                            "{name}, {height} m off, {across}, along {ray}: scatters {scattered} \
                             against {sum_scattered}"
                        );
                    }
                }
            }
            // And backing away from a lamp its light never steps.
            let over = cells[0].at;
            let mut height = 0.001_f32;
            let mut previous = panel.light(&cone, point(over, height)).white;
            while height < 12.0 {
                height *= 1.002;
                let light = panel.light(&cone, point(over, height)).white;
                assert!(
                    (light - previous).abs() <= 0.02 * light.max(previous),
                    "{name}: a step at {height} m: {previous} to {light}"
                );
                previous = light;
            }
        }
    }

    /// A face lit evenly all over has no pattern: every level of its stack
    /// reads one, wherever on the face. The buffer holds the header, the
    /// kernel's table and that stack, and a panel follows its cone to the
    /// cone's place among those drawn.
    #[test]
    fn an_even_face_has_no_pattern_and_is_packed_behind_its_cone() {
        let cells: Vec<Cell> = (0..32)
            .map(|k| Cell {
                at: Vec2::new((k % 8) as f32 - 3.5, (k / 8) as f32 - 1.5) * PITCH,
                size: Vec2::splat(PITCH),
                colour: Vec3::ONE,
                intensity: 1.0,
            })
            .collect();
        let half = Vec2::new(4.0, 2.0) * PITCH;
        let cone = cone(Vec3::ONE);
        let footprint = Footprint::of(cone.cos_beam, cone.cos_field);
        let panel = Panel::new(0, Vec3::X, (-half, half), PITCH, (&cells, 32), footprint);
        for level in 0..2 * panel.pairs() {
            for at in [Vec2::ZERO, half * 0.5, half * Vec2::new(-0.99, 0.3), -half] {
                let pattern = panel.level(level, at - panel.middle);
                assert!(
                    pattern.abs_diff_eq(glam::Vec4::ONE, 1e-3),
                    "level {level} at {at}: {pattern}"
                );
            }
        }
        // Drawn second of three cones, the first of which was dropped.
        let panel = Panel { cone: 1, ..panel };
        let (words, starts) = super::pack(std::slice::from_ref(&panel), &[1, 2]);
        assert_eq!(starts, vec![1.0, 0.0]);
        assert_eq!(
            words.len(),
            1 + 13 + 2 * panel.parts.len() + KERNEL_SIDE * KERNEL_SIDE + panel.stack.len()
        );
    }

    /// A face lit all over is lit as its cells are, one beam a cell: along
    /// the beam's middle, across its edge and past it, from touching the
    /// face to across the room.
    #[test]
    fn a_face_lit_all_over_is_the_sum_of_its_cells_beams() {
        for (beam, field) in [(5.0_f32, 9.0_f32), (10.0, 18.0), (20.0, 36.0), (50.0, 70.0)] {
            let cells: Vec<Cell> = (0..32)
                .map(|k| Cell {
                    at: Vec2::new((k % 8) as f32 - 3.5, (k / 8) as f32 - 1.5) * PITCH,
                    size: Vec2::splat(PITCH),
                    colour: Vec3::ONE,
                    intensity: 1.0,
                })
                .collect();
            let half = Vec2::new(4.0, 2.0) * PITCH;
            let mut cone = cone(Vec3::ONE);
            cone.cos_beam = beam.to_radians().cos();
            cone.cos_field = field.to_radians().cos();
            let footprint = Footprint::of(cone.cos_beam, cone.cos_field);
            let panel = Panel::new(0, Vec3::X, (-half, half), PITCH, (&cells, 32), footprint);
            // The cells tile the face, so their beams together are the
            // kernel summed over the face as the point sees it: here over a
            // grid of tangents out to the field's edge.
            let reach = field.to_radians().tan();
            let cells_send = |at: Vec2, height: f32| {
                let (lo, hi) = ((-half - at) / height, (half - at) / height);
                let (lo, hi) = (lo.max(Vec2::splat(-reach)), hi.min(Vec2::splat(reach)));
                if lo.x >= hi.x || lo.y >= hi.y {
                    return 0.0;
                }
                let steps = 200;
                let patch = (hi - lo) / steps as f32;
                let mut sum = 0.0;
                for k in 0..steps * steps {
                    let t =
                        lo + Vec2::new((k % steps) as f32 + 0.5, (k / steps) as f32 + 0.5) * patch;
                    let cos2 = 1.0 / (1.0 + t.length_squared());
                    sum += angular_profile(cos2.sqrt(), cone.cos_beam, cone.cos_field) * cos2;
                }
                sum * patch.x * patch.y / (4.0 * half.x * half.y)
            };
            for height in [0.01_f32, 0.03, 0.1, 0.3, 1.0, 3.0, 10.0] {
                let middle = cells_send(Vec2::ZERO, height);
                // Out along the face's right to twice where the beam ends.
                let ends = half.x + height * field.to_radians().tan();
                for step in 0..=16 {
                    let at = Vec2::new(ends * step as f32 / 8.0, 0.3 * half.y);
                    let (panel, cells) = (
                        panel.light(&cone, point(at, height)).white,
                        cells_send(at, height),
                    );
                    assert!(
                        (panel - cells).abs() < 0.04 * middle,
                        "{beam} degrees, {height} m off, {} m across: {panel} against {cells} \
                         ({middle} in the middle)",
                        at.x
                    );
                }
            }
        }
    }
}
