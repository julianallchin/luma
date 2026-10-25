//! What blocks the haze's glow (`haze_glow.rs`): the stage's geometry in
//! the glow's grid, as the share of straight lines along each axis that it
//! stops in each cell.
//!
//! A cell is cut into [`SUB`]³ sub-cells, and every opaque surface but the
//! fixtures' marks the sub-cells it passes through. Along x, a cell blocks
//! the share of its [`SUB`]² lines along x that cross a marked sub-cell: a
//! wall across x blocks all of them, a deck lying along x none. A coarser
//! level joins each pair of cells along an axis, a line stopped by either,
//! and averages the pairs beside each other, so a thin wall still blocks
//! all of a coarse cell's lines across it.
//!
//! Built on the CPU, only when the geometry or the grid changes, like the
//! fixtures' shadow maps.

use std::hash::{Hash, Hasher};

use glam::{UVec3, Vec3};

use crate::frame::{EditorObject, Frame};

/// Sub-cells along each axis of a glow cell.
const SUB: u32 = 4;
/// The format: the share blocked along x, y and z; alpha unused.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub(crate) struct Occupancy {
    key: Option<u64>,
    /// Every level, for the gather pass.
    pub view: wgpu::TextureView,
    pub solid: Solid,
}

/// The marked sub-cells.
pub(crate) struct Solid {
    /// Sub-cells along each axis.
    dims: UVec3,
    min: Vec3,
    /// A sub-cell's size, metres.
    sub: Vec3,
    /// One per sub-cell, x fastest: whether a surface passes through it.
    marks: Vec<bool>,
}

impl Occupancy {
    /// Nothing blocks anything.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let (_, view) = upload(device, queue, &[vec![[0; 4]]], [1, 1, 1]);
        Self {
            key: None,
            view,
            solid: Solid::empty(),
        }
    }

    /// Mark `frame`'s opaque geometry in a grid of `size` cells, `levels`
    /// deep, over `min`..`max`; nothing when none of it moved.
    pub(crate) fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &Frame,
        (min, max): (Vec3, Vec3),
        size: [u32; 3],
        levels: u32,
    ) {
        let casters = || {
            frame
                .draws
                .iter()
                .take(frame.draws.len() - frame.transparent.len())
                .filter(|d| !matches!(d.editor_object, Some(EditorObject::Fixture(_))))
        };
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        size.hash(&mut hash);
        for v in min.to_array().into_iter().chain(max.to_array()) {
            v.to_bits().hash(&mut hash);
        }
        for draw in casters() {
            frame.meshes[draw.mesh].key.hash(&mut hash);
            for v in draw.model.to_cols_array() {
                v.to_bits().hash(&mut hash);
            }
        }
        let key = hash.finish();
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.solid = Solid::new((min, max), size);
        for draw in casters() {
            let mesh = &frame.meshes[draw.mesh];
            for ids in mesh.indices.chunks_exact(3) {
                let corner = |i: usize| {
                    draw.model
                        .transform_point3(Vec3::from_array(mesh.vertices[ids[i] as usize].position))
                };
                self.solid.mark([corner(0), corner(1), corner(2)]);
            }
        }
        let mut level = self.solid.blocking(size);
        let mut sizes = vec![size];
        let mut chain = Vec::new();
        for _ in 1..levels {
            let finer = *sizes.last().unwrap_or(&size);
            let coarser = finer.map(|n| (n / 2).max(1));
            let next = join(&level, finer, coarser);
            chain.push(std::mem::replace(&mut level, next));
            sizes.push(coarser);
        }
        chain.push(level);
        let texels: Vec<Vec<[u8; 4]>> = chain
            .iter()
            .map(|level| {
                level
                    .iter()
                    .map(|b| {
                        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                        [byte(b[0]), byte(b[1]), byte(b[2]), 0]
                    })
                    .collect()
            })
            .collect();
        self.view = upload(device, queue, &texels, size).1;
    }
}

impl Solid {
    /// Nothing anywhere.
    pub(crate) fn empty() -> Self {
        Self::new((Vec3::ZERO, Vec3::ONE), [0; 3])
    }

    /// Nothing marked in a grid of `size` cells over `min`..`max`.
    fn new((min, max): (Vec3, Vec3), size: [u32; 3]) -> Self {
        let dims = UVec3::from_array(size) * SUB;
        Self {
            dims,
            min,
            sub: (max - min) / dims.max(UVec3::ONE).as_vec3(),
            marks: vec![false; dims.element_product() as usize],
        }
    }

    /// Mark the sub-cells a triangle passes through. Seen down the axis it
    /// faces most, each column of sub-cells it overlaps takes the depths
    /// its plane spans across the column, so the work is its area in
    /// sub-cells, clipped to the grid, however large it is.
    fn mark(&mut self, [a, b, c]: [Vec3; 3]) {
        if ![a, b, c].iter().all(|p| p.is_finite()) {
            return;
        }
        let normal = (b - a).cross(c - a);
        let facing = normal.abs();
        let down = if facing.x >= facing.y && facing.x >= facing.z {
            0
        } else if facing.y >= facing.z {
            1
        } else {
            2
        };
        let (u, v) = ((down + 1) % 3, (down + 2) % 3);
        if facing[down] <= 0.0 {
            return;
        }
        // In sub-cells from the grid's corner.
        let cell = |p: Vec3| (p - self.min) / self.sub;
        let [a, b, c] = [cell(a), cell(b), cell(c)];
        let normal = (b - a).cross(c - a);
        let dims = self.dims.as_vec3();
        let span = |axis: usize| {
            let low = a[axis].min(b[axis]).min(c[axis]).floor().max(0.0);
            let high = a[axis]
                .max(b[axis])
                .max(c[axis])
                .floor()
                .min(dims[axis] - 1.0);
            (low as i64, high as i64)
        };
        let ((u0, u1), (v0, v1)) = (span(u), span(v));
        let (deepest, shallowest) = (
            a[down].min(b[down]).min(c[down]),
            a[down].max(b[down]).max(c[down]),
        );
        // Each edge as a half-plane in (u, v), pushed out by half a cell's
        // reach across it, so a column the triangle only clips still counts.
        let sense = normal[down].signum();
        let edges = [(a, b), (b, c), (c, a)].map(|(p, q)| {
            let (du, dv) = (q[u] - p[u], q[v] - p[v]);
            let (nu, nv) = (-dv * sense, du * sense);
            (nu, nv, nu * p[u] + nv * p[v], 0.5 * (nu.abs() + nv.abs()))
        });
        let tilt = 0.5 * (normal[u].abs() + normal[v].abs()) / normal[down].abs();
        let mut at = UVec3::ZERO;
        for i in u0..=u1 {
            for j in v0..=v1 {
                let (pu, pv) = (i as f32 + 0.5, j as f32 + 0.5);
                if edges
                    .iter()
                    .any(|&(nu, nv, d, reach)| nu * pu + nv * pv - d < -reach)
                {
                    continue;
                }
                let depth =
                    a[down] - (normal[u] * (pu - a[u]) + normal[v] * (pv - a[v])) / normal[down];
                let low = (depth - tilt).max(deepest).floor().max(0.0);
                let high = (depth + tilt).min(shallowest).floor().min(dims[down] - 1.0);
                at[u] = i as u32;
                at[v] = j as u32;
                let mut k = low;
                while k <= high {
                    at[down] = k as u32;
                    let index = at.x + self.dims.x * (at.y + self.dims.y * at.z);
                    self.marks[index as usize] = true;
                    k += 1.0;
                }
            }
        }
    }

    /// The sub-cell `p` is in, if it is in the grid.
    fn index(&self, p: Vec3) -> Option<usize> {
        let at = ((p - self.min) / self.sub).floor();
        if at.cmplt(Vec3::ZERO).any() || at.cmpge(self.dims.as_vec3()).any() {
            return None;
        }
        let at = at.as_uvec3();
        Some((at.x + self.dims.x * (at.y + self.dims.y * at.z)) as usize)
    }

    /// Each cell's share of lines blocked along x, y and z.
    fn blocking(&self, size: [u32; 3]) -> Vec<[f32; 3]> {
        let [nx, ny, nz] = size;
        let mut out = Vec::with_capacity((nx * ny * nz) as usize);
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    // The lines each axis's marked sub-cells stop, one bit
                    // per line across the other two.
                    let mut stopped = [0u16; 3];
                    for k in 0..SUB {
                        for j in 0..SUB {
                            for i in 0..SUB {
                                let at = UVec3::new(x, y, z) * SUB + UVec3::new(i, j, k);
                                let index = at.x + self.dims.x * (at.y + self.dims.y * at.z);
                                if self.marks[index as usize] {
                                    stopped[0] |= 1 << (j + SUB * k);
                                    stopped[1] |= 1 << (i + SUB * k);
                                    stopped[2] |= 1 << (i + SUB * j);
                                }
                            }
                        }
                    }
                    out.push(stopped.map(|lines| lines.count_ones() as f32 / (SUB * SUB) as f32));
                }
            }
        }
        out
    }

    /// How far a beam from `origin` along unit `direction` goes, at most
    /// `range`, before it meets a surface. A projector that sits on or in
    /// something is not stopped by it: only a surface after the beam is
    /// clear counts.
    pub(crate) fn reach(&self, origin: Vec3, direction: Vec3, range: f32) -> f32 {
        if self.marks.is_empty() {
            return range;
        }
        let step = 0.5 * self.sub.min_element();
        let mut clear = false;
        let mut travelled = 0.0;
        while travelled < range {
            let solid = self
                .index(origin + direction * travelled)
                .is_some_and(|i| self.marks[i]);
            if solid && clear {
                return travelled;
            }
            clear |= !solid;
            travelled += step;
        }
        range
    }
}

/// The next coarser level: a line along an axis is stopped by either cell of
/// a pair along it, and the pairs beside each other average.
fn join(finer: &[[f32; 3]], size: [u32; 3], coarser: [u32; 3]) -> Vec<[f32; 3]> {
    let at = |x: u32, y: u32, z: u32| {
        let [x, y, z] = [x.min(size[0] - 1), y.min(size[1] - 1), z.min(size[2] - 1)];
        finer[(x + size[0] * (y + size[1] * z)) as usize]
    };
    let mut out = Vec::with_capacity(coarser.iter().product::<u32>() as usize);
    for z in 0..coarser[2] {
        for y in 0..coarser[1] {
            for x in 0..coarser[0] {
                let mut blocked = [0.0; 3];
                for axis in 0..3 {
                    for side in 0..4u32 {
                        let mut pair = [1.0f32; 2];
                        for (along, through) in pair.iter_mut().enumerate() {
                            let mut child = [2 * x, 2 * y, 2 * z];
                            child[axis] += along as u32;
                            child[(axis + 1) % 3] += side & 1;
                            child[(axis + 2) % 3] += side >> 1;
                            *through = 1.0 - at(child[0], child[1], child[2])[axis];
                        }
                        blocked[axis] += 0.25 * (1.0 - pair[0] * pair[1]);
                    }
                }
                out.push(blocked);
            }
        }
    }
    out
}

fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    levels: &[Vec<[u8; 4]>],
    size: [u32; 3],
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glow-occupancy"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: size[2],
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, texels) in levels.iter().enumerate() {
        let [w, h, d] = size.map(|n| (n >> level).max(1));
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(texels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: d,
            },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two cells along each axis, a metre each, with a wall a sub-cell
    /// thick across x through the middle of the first.
    fn walled() -> Solid {
        let mut solid = Solid::new((Vec3::ZERO, Vec3::splat(2.0)), [2; 3]);
        for k in 0..2 * SUB {
            for j in 0..2 * SUB {
                let at = Vec3::new(0.4, j as f32, k as f32) * solid.sub + 0.5 * solid.sub;
                let i = solid.index(at).unwrap();
                solid.marks[i] = true;
            }
        }
        solid
    }

    /// A wall drawn as two triangles, tilted a little, marks a wall: every
    /// line along x through its cells stops, and the cells beside it stay
    /// open.
    #[test]
    fn triangles_mark_what_they_pass_through() {
        let mut solid = Solid::new((Vec3::ZERO, Vec3::splat(2.0)), [2; 3]);
        let corners = [
            Vec3::new(0.3, -1.0, -1.0),
            Vec3::new(0.4, 3.0, -1.0),
            Vec3::new(0.4, 3.0, 3.0),
            Vec3::new(0.3, -1.0, 3.0),
        ];
        solid.mark([corners[0], corners[1], corners[2]]);
        solid.mark([corners[0], corners[2], corners[3]]);
        let blocking = solid.blocking([2; 3]);
        for (i, cell) in blocking.iter().enumerate() {
            if i % 2 == 0 {
                assert_eq!(cell[0], 1.0, "{i}: {cell:?}");
            } else {
                assert_eq!(*cell, [0.0; 3], "{i}");
            }
        }
    }

    /// A wall across x stops every line along x through its cell, and only
    /// the lines along y and z that lie in it.
    #[test]
    fn a_wall_blocks_the_lines_across_it() {
        let blocking = walled().blocking([2; 3]);
        assert_eq!(blocking[0][0], 1.0);
        assert!(
            blocking[0][1] < 0.5 && blocking[0][2] < 0.5,
            "{:?}",
            blocking[0]
        );
        assert_eq!(blocking[1], [0.0; 3], "the cell beside it is open");
    }

    /// A thin wall still blocks all of a coarser cell's lines across it,
    /// and few of those along it.
    #[test]
    fn a_coarse_cell_keeps_a_thin_wall() {
        let coarse = join(&walled().blocking([2; 3]), [2; 3], [1; 3]);
        assert_eq!(coarse[0][0], 1.0);
        assert!(coarse[0][1] < 0.5 && coarse[0][2] < 0.5, "{:?}", coarse[0]);
    }

    /// A beam stops at the first surface it meets, not at the one its
    /// projector stands in.
    #[test]
    fn a_beam_reaches_the_wall() {
        let solid = walled();
        let reach = solid.reach(Vec3::new(1.9, 0.5, 0.5), Vec3::NEG_X, 80.0);
        assert!((reach - 1.7).abs() < 0.15, "{reach}");
        let inside = solid.reach(Vec3::new(0.15, 0.5, 0.5), Vec3::X, 80.0);
        assert_eq!(inside, 80.0, "from inside the wall, out into the open");
        let along = solid.reach(Vec3::new(1.5, 0.5, 0.5), Vec3::Y, 80.0);
        assert_eq!(along, 80.0, "beside the wall, nothing in the way");
    }
}
