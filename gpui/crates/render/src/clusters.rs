//! Deterministic CPU construction of clustered finite-cone light lists.
//!
//! This module owns only the spatial index. GPU buffer layout and shader
//! consumption deliberately live elsewhere, so the same builder can be tested
//! without an adapter or graphics device.

use std::fmt;

use glam::Vec3;

use crate::frame::{Camera, FixtureCone};

/// Width and height, in pixels, of one screen-space cluster tile.
pub const CLUSTER_TILE_SIZE: u32 = 32;
/// Number of logarithmic view-depth slices in every cluster grid.
pub const CLUSTER_DEPTH_SLICES: u32 = 16;

const MAX_CLUSTERS: usize = 16 * 1024 * 1024;
const EYE_EPSILON: f32 = 1.0e-4;

/// Offset and count of one cluster's entries in [`ClusterGrid::light_indices`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClusterHeader {
    /// First packed light index.
    pub offset: u32,
    /// Number of packed light indices.
    pub count: u32,
}

/// A deterministic compressed-sparse-row light index.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterGrid {
    /// Number of X tiles.
    pub columns: u32,
    /// Number of Y tiles.
    pub rows: u32,
    /// One header per `(z * rows + y) * columns + x` cluster.
    pub headers: Vec<ClusterHeader>,
    /// Fixture-cone indices, in source order, packed for all clusters.
    pub light_indices: Vec<u32>,
    near: f32,
    far: f32,
}

impl ClusterGrid {
    /// Returns the flattened cluster index for a pixel and positive view depth.
    ///
    /// Coordinates outside the viewport's edge tiles are clamped. Non-finite
    /// depths map to the nearest slice, keeping callers within bounds.
    #[must_use]
    pub fn cluster_index(&self, pixel_x: u32, pixel_y: u32, view_depth: f32) -> usize {
        let x = (pixel_x / CLUSTER_TILE_SIZE).min(self.columns - 1);
        let y = (pixel_y / CLUSTER_TILE_SIZE).min(self.rows - 1);
        let z = depth_slice(view_depth, self.near, self.far);
        ((z * self.rows + y) * self.columns + x) as usize
    }

    /// Returns the source light indices assigned to a flattened cluster.
    #[must_use]
    pub fn lights(&self, cluster: usize) -> &[u32] {
        let Some(header) = self.headers.get(cluster) else {
            return &[];
        };
        let start = header.offset as usize;
        let end = start.saturating_add(header.count as usize);
        self.light_indices.get(start..end).unwrap_or_default()
    }

    /// Near plane used to build this grid.
    #[must_use]
    pub const fn near(&self) -> f32 {
        self.near
    }

    /// Far plane used to build this grid.
    #[must_use]
    pub const fn far(&self) -> f32 {
        self.far
    }
}

/// Invalid dimensions that would make the CSR allocation unsafe or unreasonable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterBuildError;

impl fmt::Display for ClusterBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("cluster grid dimensions exceed the safe allocation limit")
    }
}

impl std::error::Error for ClusterBuildError {}

/// Cache for cluster lists whose key excludes shading-only fixture state.
#[derive(Debug, Default)]
pub struct ClusterCache {
    key: Option<ClusterCacheKey>,
    grid: Option<ClusterGrid>,
}

impl ClusterCache {
    /// Returns a cached grid or rebuilds it when camera, viewport, or cone
    /// topology changed. The boolean is true exactly when a rebuild occurred.
    ///
    /// # Errors
    ///
    /// Returns [`ClusterBuildError`] when the viewport would require an unsafe
    /// or unreasonably large cluster allocation.
    pub fn get_or_build(
        &mut self,
        lights: &[FixtureCone],
        camera: Camera,
        viewport: [u32; 2],
        near: f32,
        far: f32,
    ) -> Result<(&ClusterGrid, bool), ClusterBuildError> {
        let input = BuildInput::new(camera, viewport, near, far)?;
        let key = input.cache_key().with_lights(lights);
        let rebuilt = self.key.as_ref() != Some(&key);
        if rebuilt {
            self.grid = Some(input.build(lights)?);
            self.key = Some(key);
        }
        self.grid
            .as_ref()
            .map(|grid| (grid, rebuilt))
            .ok_or(ClusterBuildError)
    }
}

/// Builds a fresh clustered light index.
///
/// # Errors
///
/// Returns [`ClusterBuildError`] when the viewport would require an unsafe or
/// unreasonably large cluster allocation.
pub fn build_clusters(
    lights: &[FixtureCone],
    camera: Camera,
    viewport: [u32; 2],
    near: f32,
    far: f32,
) -> Result<ClusterGrid, ClusterBuildError> {
    BuildInput::new(camera, viewport, near, far)?.build(lights)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClusterCacheKey {
    viewport: [u32; 2],
    near_far: [u32; 2],
    camera: [u32; 7],
    topology_hash: u64,
}

#[derive(Clone, Copy)]
struct Basis {
    eye: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
    tan_half_fov: f32,
    aspect: f32,
}

struct BuildInput {
    viewport: [u32; 2],
    columns: u32,
    rows: u32,
    cluster_count: usize,
    near: f32,
    far: f32,
    camera: Camera,
    basis: Basis,
}

impl BuildInput {
    fn new(
        camera: Camera,
        viewport: [u32; 2],
        near: f32,
        far: f32,
    ) -> Result<Self, ClusterBuildError> {
        let viewport = [viewport[0].max(1), viewport[1].max(1)];
        let columns = viewport[0].div_ceil(CLUSTER_TILE_SIZE);
        let rows = viewport[1].div_ceil(CLUSTER_TILE_SIZE);
        let cluster_count = usize::try_from(columns)
            .ok()
            .and_then(|value| value.checked_mul(rows as usize))
            .and_then(|value| value.checked_mul(CLUSTER_DEPTH_SLICES as usize))
            .filter(|&value| value <= MAX_CLUSTERS)
            .ok_or(ClusterBuildError)?;

        let near = finite(near, 0.05).clamp(0.001, 10_000.0);
        let far = finite(far, near + 1.0).clamp(near + 0.001, 100_000.0);
        let camera = sanitize_camera(camera);
        let forward = (camera.target - camera.eye).normalize_or(Vec3::Y);
        let world_up = if forward.z.abs() > 0.99 {
            Vec3::Y
        } else {
            Vec3::Z
        };
        let right = forward.cross(world_up).normalize_or(Vec3::X);
        let up = right.cross(forward).normalize_or(Vec3::Z);
        let basis = Basis {
            eye: camera.eye,
            right,
            up,
            forward,
            tan_half_fov: (camera.fov_y_deg.to_radians() * 0.5).tan(),
            aspect: viewport[0] as f32 / viewport[1] as f32,
        };
        Ok(Self {
            viewport,
            columns,
            rows,
            cluster_count,
            near,
            far,
            camera,
            basis,
        })
    }

    fn cache_key(&self) -> ClusterCacheKey {
        ClusterCacheKey {
            viewport: self.viewport,
            near_far: [self.near.to_bits(), self.far.to_bits()],
            camera: [
                self.camera.eye.x.to_bits(),
                self.camera.eye.y.to_bits(),
                self.camera.eye.z.to_bits(),
                self.camera.target.x.to_bits(),
                self.camera.target.y.to_bits(),
                self.camera.target.z.to_bits(),
                self.camera.fov_y_deg.to_bits(),
            ],
            // Filled by `key_for_lights`; retained here to keep construction
            // and allocation validation together.
            topology_hash: 0,
        }
    }

    fn build(&self, lights: &[FixtureCone]) -> Result<ClusterGrid, ClusterBuildError> {
        let bounds = lights
            .iter()
            .map(|light| self.bounds_for(light))
            .collect::<Vec<_>>();

        // Pass one counts memberships. Keeping only one u32 per cluster avoids
        // tens of thousands of tiny `Vec` allocations at viewport resolution.
        let mut counts = vec![0_u32; self.cluster_count];
        for bounds in bounds.iter().flatten() {
            for z in bounds.z0..=bounds.z1 {
                for y in bounds.y0..=bounds.y1 {
                    for x in bounds.x0..=bounds.x1 {
                        let cluster = ((z * self.rows + y) * self.columns + x) as usize;
                        counts[cluster] =
                            counts[cluster].checked_add(1).ok_or(ClusterBuildError)?;
                    }
                }
            }
        }

        // Prefix counts into immutable CSR headers and allocate the packed list
        // exactly once. u32 offsets mirror the intended storage-buffer layout.
        let mut headers = Vec::with_capacity(counts.len());
        let mut packed_len = 0_u32;
        for count in counts {
            headers.push(ClusterHeader {
                offset: packed_len,
                count,
            });
            packed_len = packed_len.checked_add(count).ok_or(ClusterBuildError)?;
        }
        let mut light_indices = vec![0_u32; packed_len as usize];
        let mut cursors = headers
            .iter()
            .map(|header| header.offset)
            .collect::<Vec<_>>();

        // Pass two visits lights in source order, making every cluster's list
        // sorted and the byte representation reproducible across builds.
        for (light_index, bounds) in bounds.iter().enumerate() {
            let Some(bounds) = bounds else { continue };
            let light_index = u32::try_from(light_index).map_err(|_| ClusterBuildError)?;
            for z in bounds.z0..=bounds.z1 {
                for y in bounds.y0..=bounds.y1 {
                    for x in bounds.x0..=bounds.x1 {
                        let cluster = ((z * self.rows + y) * self.columns + x) as usize;
                        let destination = cursors[cluster] as usize;
                        light_indices[destination] = light_index;
                        cursors[cluster] += 1;
                    }
                }
            }
        }
        debug_assert!(headers
            .iter()
            .zip(&cursors)
            .all(|(header, &cursor)| cursor == header.offset + header.count));

        Ok(ClusterGrid {
            columns: self.columns,
            rows: self.rows,
            headers,
            light_indices,
            near: self.near,
            far: self.far,
        })
    }

    fn bounds_for(&self, light: &FixtureCone) -> Option<ClusterBounds> {
        let cone = SanitizedCone::new(light);
        let base = cone.position + cone.direction * cone.range;
        let radial =
            cone.range * (1.0 - cone.cos_field * cone.cos_field).max(0.0).sqrt() / cone.cos_field;
        let radial_extent = Vec3::new(
            (1.0 - cone.direction.x * cone.direction.x).max(0.0).sqrt(),
            (1.0 - cone.direction.y * cone.direction.y).max(0.0).sqrt(),
            (1.0 - cone.direction.z * cone.direction.z).max(0.0).sqrt(),
        ) * radial;
        let min = cone.position.min(base - radial_extent);
        let max = cone.position.max(base + radial_extent);

        let mut min_pixel = [f32::INFINITY; 2];
        let mut max_pixel = [f32::NEG_INFINITY; 2];
        let mut min_depth = f32::INFINITY;
        let mut max_depth = f32::NEG_INFINITY;
        let mut behind_eye = false;
        let mut in_front = false;
        for z in [min.z, max.z] {
            for y in [min.y, max.y] {
                for x in [min.x, max.x] {
                    let relative = Vec3::new(x, y, z) - self.basis.eye;
                    let depth = relative.dot(self.basis.forward);
                    min_depth = min_depth.min(depth);
                    max_depth = max_depth.max(depth);
                    if depth <= EYE_EPSILON {
                        behind_eye = true;
                    } else {
                        in_front = true;
                        let ndc_x = relative.dot(self.basis.right)
                            / (depth * self.basis.tan_half_fov * self.basis.aspect);
                        let ndc_y = relative.dot(self.basis.up) / (depth * self.basis.tan_half_fov);
                        let pixel_x = (ndc_x * 0.5 + 0.5) * self.viewport[0] as f32;
                        let pixel_y = (0.5 - ndc_y * 0.5) * self.viewport[1] as f32;
                        min_pixel[0] = min_pixel[0].min(pixel_x);
                        min_pixel[1] = min_pixel[1].min(pixel_y);
                        max_pixel[0] = max_pixel[0].max(pixel_x);
                        max_pixel[1] = max_pixel[1].max(pixel_y);
                    }
                }
            }
        }
        if !in_front || max_depth < self.near || min_depth > self.far {
            return None;
        }
        let z0 = depth_slice(min_depth.max(self.near), self.near, self.far);
        let z1 = depth_slice(max_depth.min(self.far), self.near, self.far);
        let (x0, y0, x1, y1) = if behind_eye {
            (0, 0, self.columns - 1, self.rows - 1)
        } else {
            if max_pixel[0] < 0.0
                || max_pixel[1] < 0.0
                || min_pixel[0] >= self.viewport[0] as f32
                || min_pixel[1] >= self.viewport[1] as f32
            {
                return None;
            }
            (
                pixel_tile(min_pixel[0], self.columns),
                pixel_tile(min_pixel[1], self.rows),
                pixel_tile(max_pixel[0], self.columns),
                pixel_tile(max_pixel[1], self.rows),
            )
        };
        Some(ClusterBounds {
            x0,
            y0,
            z0,
            x1,
            y1,
            z1,
        })
    }
}

impl ClusterCacheKey {
    fn with_lights(mut self, lights: &[FixtureCone]) -> Self {
        self.topology_hash = topology_hash(lights);
        self
    }
}

fn topology_hash(lights: &[FixtureCone]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut push = |value: u32| {
        hash = (hash ^ u64::from(value)).wrapping_mul(0x0000_0100_0000_01b3);
    };
    push(lights.len() as u32);
    for light in lights {
        let light = SanitizedCone::new(light);
        for value in light.position.to_array() {
            push(value.to_bits());
        }
        push(light.range.to_bits());
        for value in light.direction.to_array() {
            push(value.to_bits());
        }
        push(light.cos_field.to_bits());
    }
    hash
}

#[derive(Clone, Copy)]
struct SanitizedCone {
    position: Vec3,
    range: f32,
    direction: Vec3,
    cos_field: f32,
}

impl SanitizedCone {
    fn new(light: &FixtureCone) -> Self {
        Self {
            position: finite_vec(light.position, Vec3::ZERO)
                .clamp(Vec3::splat(-100_000.0), Vec3::splat(100_000.0)),
            range: finite(light.range, 0.05).clamp(0.05, 10_000.0),
            direction: finite_vec(light.direction, Vec3::NEG_Z)
                .try_normalize()
                .unwrap_or(Vec3::NEG_Z),
            cos_field: finite(light.cos_field, 0.95).clamp(0.01, 1.0),
        }
    }
}

#[derive(Clone, Copy)]
struct ClusterBounds {
    x0: u32,
    y0: u32,
    z0: u32,
    x1: u32,
    y1: u32,
    z1: u32,
}

fn sanitize_camera(camera: Camera) -> Camera {
    let eye =
        finite_vec(camera.eye, Vec3::ZERO).clamp(Vec3::splat(-100_000.0), Vec3::splat(100_000.0));
    let mut target = finite_vec(camera.target, eye + Vec3::Y)
        .clamp(Vec3::splat(-100_000.0), Vec3::splat(100_000.0));
    if target.distance_squared(eye) < 1.0e-8 {
        target = eye + Vec3::Y;
    }
    Camera {
        eye,
        target,
        fov_y_deg: finite(camera.fov_y_deg, 60.0).clamp(1.0, 179.0),
    }
}

fn finite(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

fn finite_vec(value: Vec3, fallback: Vec3) -> Vec3 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

fn pixel_tile(pixel: f32, tile_count: u32) -> u32 {
    ((pixel.max(0.0) as u32) / CLUSTER_TILE_SIZE).min(tile_count - 1)
}

fn depth_slice(depth: f32, near: f32, far: f32) -> u32 {
    let depth = finite(depth, near).clamp(near, far);
    let normalized = (depth / near).ln() / (far / near).ln();
    (normalized * CLUSTER_DEPTH_SLICES as f32)
        .floor()
        .clamp(0.0, (CLUSTER_DEPTH_SLICES - 1) as f32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        Camera {
            eye: Vec3::ZERO,
            target: Vec3::Y,
            fov_y_deg: 60.0,
        }
    }

    fn cone(position: Vec3, direction: Vec3, range: f32, cos_field: f32) -> FixtureCone {
        FixtureCone {
            position,
            range,
            direction,
            cos_beam: (cos_field + 0.05).min(1.0),
            color: Vec3::ONE,
            intensity: 1.0,
            cos_field,
            wash: 0.0,
            gobo: 0,
            gobo_rotation: 0.0,
        }
    }

    #[test]
    fn csr_is_in_bounds_ordered_and_repeatable() {
        let lights = [
            cone(Vec3::new(-1.0, 4.0, 0.0), Vec3::Y, 3.0, 0.9),
            cone(Vec3::new(1.0, 6.0, 0.0), Vec3::Y, 2.0, 0.8),
            cone(Vec3::new(0.0, 2.0, 1.0), Vec3::Y, 5.0, 0.95),
        ];
        let first = build_clusters(&lights, camera(), [641, 359], 0.1, 100.0).unwrap();
        let second = build_clusters(&lights, camera(), [641, 359], 0.1, 100.0).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.headers.len(), 21 * 12 * 16);
        let mut expected_offset = 0;
        for header in &first.headers {
            assert_eq!(header.offset as usize, expected_offset);
            let end = header.offset as usize + header.count as usize;
            assert!(end <= first.light_indices.len());
            assert!(first.light_indices[header.offset as usize..end]
                .windows(2)
                .all(|pair| pair[0] < pair[1]));
            expected_offset = end;
        }
        assert_eq!(expected_offset, first.light_indices.len());
        assert_eq!(
            first.light_indices.len(),
            first
                .headers
                .iter()
                .map(|header| header.count as usize)
                .sum::<usize>()
        );
        assert_eq!(first.light_indices.capacity(), first.light_indices.len());
        assert!(first.light_indices.iter().all(|&index| index < 3));
    }

    #[test]
    fn isolated_cones_are_local_not_global() {
        let lights = [cone(
            Vec3::new(0.0, 8.0, 0.0),
            Vec3::new(0.0, 1.0, -0.1).normalize(),
            2.0,
            0.97,
        )];
        let grid = build_clusters(&lights, camera(), [1280, 720], 0.1, 100.0).unwrap();
        let occupied = grid
            .headers
            .iter()
            .filter(|header| header.count > 0)
            .count();
        assert!(occupied > 0);
        assert!(occupied < grid.headers.len() / 20, "occupied {occupied}");
    }

    #[test]
    fn shading_only_changes_reuse_cache() {
        let mut cache = ClusterCache::default();
        let mut light = cone(Vec3::new(0.0, 4.0, 0.0), Vec3::Y, 4.0, 0.9);
        let (_, rebuilt) = cache
            .get_or_build(&[light], camera(), [640, 360], 0.1, 100.0)
            .unwrap();
        assert!(rebuilt);
        light.color = Vec3::new(10.0, 0.2, 3.0);
        light.intensity = 42.0;
        light.cos_beam = 0.4;
        light.wash = 1.0;
        light.gobo = 2;
        light.gobo_rotation = 1.7;
        let (_, rebuilt) = cache
            .get_or_build(&[light], camera(), [640, 360], 0.1, 100.0)
            .unwrap();
        assert!(!rebuilt);
        light.position.x += 0.1;
        let (_, rebuilt) = cache
            .get_or_build(&[light], camera(), [640, 360], 0.1, 100.0)
            .unwrap();
        assert!(rebuilt);
    }

    #[test]
    fn topology_hash_is_stable_order_sensitive_and_shading_blind() {
        let first = cone(Vec3::new(1.0, 4.0, 2.0), Vec3::Y, 4.0, 0.9);
        let second = cone(Vec3::new(-2.0, 7.0, 1.0), Vec3::Z, 2.0, 0.8);
        let baseline = topology_hash(&[first, second]);
        assert_eq!(baseline, topology_hash(&[first, second]));
        assert_ne!(baseline, topology_hash(&[second, first]));

        let mut shaded = first;
        shaded.color = Vec3::splat(9.0);
        shaded.intensity = 0.0;
        shaded.cos_beam = 0.2;
        shaded.wash = 1.0;
        shaded.gobo = 2;
        shaded.gobo_rotation = 4.0;
        assert_eq!(baseline, topology_hash(&[shaded, second]));
    }

    #[test]
    fn cache_changes_for_camera_projection_and_viewport() {
        let light = cone(Vec3::new(0.0, 4.0, 0.0), Vec3::Y, 4.0, 0.9);
        let mut cache = ClusterCache::default();
        cache
            .get_or_build(&[light], camera(), [640, 360], 0.1, 100.0)
            .unwrap();
        let mut moved = camera();
        moved.eye.x = 0.1;
        assert!(
            cache
                .get_or_build(&[light], moved, [640, 360], 0.1, 100.0)
                .unwrap()
                .1
        );
        assert!(
            cache
                .get_or_build(&[light], moved, [641, 360], 0.1, 100.0)
                .unwrap()
                .1
        );
        assert!(
            cache
                .get_or_build(&[light], moved, [641, 360], 0.2, 100.0)
                .unwrap()
                .1
        );
    }

    #[test]
    fn eye_plane_crossing_expands_xy() {
        let light = cone(Vec3::new(0.0, -0.1, 0.0), Vec3::Y, 1.0, 0.8);
        let grid = build_clusters(&[light], camera(), [320, 192], 0.01, 10.0).unwrap();
        let covered_per_slice = grid
            .headers
            .chunks((grid.columns * grid.rows) as usize)
            .filter(|slice| slice.iter().any(|header| header.count > 0));
        for slice in covered_per_slice {
            assert!(slice.iter().all(|header| header.count == 1));
        }
    }

    #[test]
    fn rejects_pathological_allocations_and_safely_handles_oob_queries() {
        assert!(build_clusters(&[], camera(), [u32::MAX, u32::MAX], 0.1, 100.0).is_err());
        let grid = build_clusters(&[], camera(), [0, 0], 0.1, 100.0).unwrap();
        assert_eq!(grid.headers.len(), 16);
        assert!(grid.lights(usize::MAX).is_empty());
        assert!(grid.cluster_index(u32::MAX, u32::MAX, f32::INFINITY) < grid.headers.len());
    }

    #[test]
    fn non_finite_inputs_are_sanitized_deterministically() {
        let bad = cone(
            Vec3::new(f32::NAN, f32::INFINITY, f32::NEG_INFINITY),
            Vec3::splat(f32::NAN),
            f32::INFINITY,
            f32::NAN,
        );
        let bad_camera = Camera {
            eye: Vec3::splat(f32::NAN),
            target: Vec3::splat(f32::INFINITY),
            fov_y_deg: f32::NAN,
        };
        let first =
            build_clusters(&[bad], bad_camera, [320, 180], f32::NAN, f32::INFINITY).unwrap();
        let second =
            build_clusters(&[bad], bad_camera, [320, 180], f32::NAN, f32::INFINITY).unwrap();
        assert_eq!(first, second);
        assert!(first.light_indices.iter().all(|&index| index == 0));
    }

    #[test]
    fn sampled_cone_points_never_land_in_an_unlisted_cluster() {
        let mut state = 0x5eed_1234_u64;
        let mut random = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state as u32) as f32 / u32::MAX as f32
        };
        let mut lights = Vec::new();
        for _ in 0..96 {
            let position = Vec3::new(
                (random() - 0.5) * 18.0,
                0.02 + random() * 40.0,
                (random() - 0.5) * 14.0,
            );
            let direction =
                Vec3::new(random() - 0.5, random() * 0.8 + 0.2, random() - 0.5).normalize();
            lights.push(cone(
                position,
                direction,
                0.25 + random() * 12.0,
                0.55 + random() * 0.44,
            ));
        }
        let viewport = [960, 544];
        let grid = build_clusters(&lights, camera(), viewport, 0.01, 100.0).unwrap();
        let input = BuildInput::new(camera(), viewport, 0.01, 100.0).unwrap();
        for (index, original) in lights.iter().enumerate() {
            let light = SanitizedCone::new(original);
            let tangent = light.direction.any_orthonormal_vector();
            let bitangent = light.direction.cross(tangent);
            for axial_step in 0..=12 {
                let axial = light.range * axial_step as f32 / 12.0;
                let radius =
                    axial * (1.0 - light.cos_field * light.cos_field).sqrt() / light.cos_field;
                for radial_fraction in [0.0_f32, 0.5, 1.0] {
                    for ring_step in 0..16 {
                        let angle = std::f32::consts::TAU * ring_step as f32 / 16.0;
                        let point = light.position
                            + light.direction * axial
                            + (tangent * angle.cos() + bitangent * angle.sin())
                                * radius
                                * radial_fraction;
                        let relative = point - input.basis.eye;
                        let depth = relative.dot(input.basis.forward);
                        if !(input.near..=input.far).contains(&depth) {
                            continue;
                        }
                        let ndc_x = relative.dot(input.basis.right)
                            / (depth * input.basis.tan_half_fov * input.basis.aspect);
                        let ndc_y =
                            relative.dot(input.basis.up) / (depth * input.basis.tan_half_fov);
                        if !(-1.0..=1.0).contains(&ndc_x) || !(-1.0..=1.0).contains(&ndc_y) {
                            continue;
                        }
                        let pixel_x = ((ndc_x * 0.5 + 0.5) * viewport[0] as f32)
                            .floor()
                            .min((viewport[0] - 1) as f32)
                            as u32;
                        let pixel_y = ((0.5 - ndc_y * 0.5) * viewport[1] as f32)
                            .floor()
                            .min((viewport[1] - 1) as f32)
                            as u32;
                        let cluster = grid.cluster_index(pixel_x, pixel_y, depth);
                        assert!(
                            grid.lights(cluster).contains(&(index as u32)),
                            "cone {index} omitted at {point:?}, pixel ({pixel_x},{pixel_y}), depth {depth}"
                        );
                    }
                }
            }
        }
    }
}
