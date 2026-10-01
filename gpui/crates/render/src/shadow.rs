//! Per-fixture shadow maps: which cones deserve one, how each is projected,
//! and when a rendered map is still valid.
//!
//! Pure decisions and identity — slot assignment, projection matrices, cache
//! keys, and the atlas allocation. The passes that render into these maps and
//! the buffers that carry the results live in [`crate::gpu`].

use glam::{Mat4, Vec3};

use crate::frame::{FixtureCone, Frame};
use crate::gpu::DEPTH_FORMAT;

/// Spotlight shadows are deliberately numerous and compact. A 256² layer is
/// enough for the soft-edged occluders visible inside haze, while 128 layers
/// cost 32 MiB instead of multiplying the directional cascade allocation.
pub(crate) const FIXTURE_SHADOW_SIZE: u32 = 256;

/// Legacy comparison mode's map budget and the minimum cache allocation.
/// All-fixture mode grows retained maps to the light count (up to 512) and
/// redraws only changed projections or geometry.
pub(crate) const MAX_FIXTURE_SHADOWS: usize = 16;

/// Identity of a rendered shadow map: the matrix it was projected with and the
/// geometry that cast into it. Shared by the sun cascades and the per-fixture
/// maps — the question "is this depth map still valid" is the same for both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShadowCacheKey {
    pub(crate) matrix_bits: [u32; 16],
    pub(crate) caster_hash: u64,
}

/// Near/far planes of a cone's shadow projection, measured from the virtual
/// apex. One home: the matrix and the shader-side depth linearisation must
/// agree on these numbers exactly.
///
/// The near plane sits at the lens: nothing between the virtual apex and the
/// lens — the inside of the fixture — casts into its own beam. A lens closer to
/// the apex than the precision floor keeps the floor, as a point source does.
/// The far plane is the lens range carried back to the apex.
pub(crate) fn fixture_shadow_planes(light: &FixtureCone) -> (f32, f32) {
    let reach = light.range.clamp(0.05, 100.0);
    let lens = light.lens_distance();
    let near = (reach * 0.0025).clamp(0.01, 0.1).min(reach * 0.5).max(lens);
    (near, reach + lens)
}

pub(crate) fn fixture_shadow_matrix(light: &FixtureCone) -> Mat4 {
    let direction = light.direction.try_normalize().unwrap_or(Vec3::NEG_Y);
    let up = if direction.dot(Vec3::Z).abs() > 0.95 {
        Vec3::Y
    } else {
        Vec3::Z
    };
    // Projected from the virtual apex, so the map's frustum is the beam's own
    // cone: lens-wide at the near plane.
    let apex = light.apex();
    let view = Mat4::look_at_rh(apex, apex + direction, up);
    let field = (2.0 * light.cos_field.clamp(-0.98, 0.9999).acos())
        .clamp(1.0_f32.to_radians(), 170.0_f32.to_radians());
    let (near, far) = fixture_shadow_planes(light);
    // Reverse-Z, matching every other depth target in this renderer.
    Mat4::perspective_rh(field, 1.0, far, near) * view
}

/// How much a cone's shadow is noticed: its apparent size from the eye scaled
/// by how bright it is. A close, wide, intense beam ranks first.
pub(crate) fn shadow_priority(cone: &FixtureCone, eye: Vec3) -> f32 {
    let half_angle_tan =
        (1.0 - cone.cos_field * cone.cos_field).max(0.0).sqrt() / cone.cos_field.max(1.0e-3);
    let radius = cone.range * half_angle_tan;
    let distance = (cone.position - eye).length().max(0.1);
    cone.intensity.max(0.0) * (radius * radius) / (distance * distance)
}

/// Choose which cones get a shadow map this frame, keeping last frame's choice
/// where it is still defensible.
///
/// Priority is the cone's apparent size from the eye scaled by how bright it
/// is: a close, wide, intense beam is the one whose missing shadow would be
/// noticed. `previous` holds the cone index resident in each slot, and a
/// resident is only evicted by a challenger clearly better than it — without
/// that margin two cones of nearly equal priority trade the slot every frame
/// and their shadows flicker.
///
/// Only scattering cones compete for the limited shadow slots, including
/// house lamps. Ranking and hysteresis keep the most visible shadows stable.
pub(crate) fn assign_shadow_slots(
    cones: &[FixtureCone],
    eye: Vec3,
    previous: &[Option<usize>; MAX_FIXTURE_SHADOWS],
) -> [Option<usize>; MAX_FIXTURE_SHADOWS] {
    /// How much better a challenger must be to take an occupied slot.
    const EVICTION_MARGIN: f32 = 1.25;

    let priority = |cone: &FixtureCone| shadow_priority(cone, eye);

    let mut ranked: Vec<(usize, f32)> = cones
        .iter()
        .enumerate()
        .filter(|(_, cone)| cone.haze_gain > 0.0)
        .map(|(index, cone)| (index, priority(cone)))
        .filter(|(_, score)| *score > 0.0)
        .collect();
    // Descending by score, index breaking ties so the assignment is
    // deterministic for a given frame.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

    let score_of = |index: usize| ranked.iter().find(|(i, _)| *i == index).map(|(_, s)| *s);
    let mut slots = *previous;
    // Drop residents that have gone dark or vanished from the frame.
    for slot in &mut slots {
        if slot.is_some_and(|index| score_of(index).is_none()) {
            *slot = None;
        }
    }
    for (index, score) in ranked.iter().copied() {
        if slots.contains(&Some(index)) {
            continue;
        }
        let Some((slot, resident_score)) = slots
            .iter()
            .enumerate()
            .map(|(slot, resident)| {
                (
                    slot,
                    resident.and_then(score_of).unwrap_or(f32::NEG_INFINITY),
                )
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
        else {
            break;
        };
        if resident_score.is_finite() && score < resident_score * EVICTION_MARGIN {
            // The weakest resident is holding its own; nothing below this
            // challenger can displace it either.
            break;
        }
        slots[slot] = Some(index);
    }
    slots
}

/// Retain visibility by projection and caster geometry, not the compacted
/// light-list index. Colour/dimmer changes and a blackout must not evict a
/// stationary emitter's already rendered depth map.
///
/// A cone whose projection changed goes back to the free slot last drawn from
/// its own position (`origins`), so a moving head keeps its previous map. That
/// is what lets [`budget_redraws`] defer the redraw and keep showing it.
pub(crate) fn assign_cached_slots(
    cones: &[FixtureCone],
    cache: &[Option<ShadowCacheKey>],
    origins: &[Option<Vec3>],
    caster_hash: u64,
) -> Vec<Option<usize>> {
    let keys: Vec<_> = cones
        .iter()
        .map(|cone| ShadowCacheKey {
            matrix_bits: shadow_matrix_bits(&fixture_shadow_matrix(cone).to_cols_array_2d()),
            caster_hash,
        })
        .collect();
    let mut slots = vec![None; cache.len()];
    let mut pending = Vec::new();
    for (index, cone) in cones.iter().enumerate() {
        if cone.intensity <= 0.0 {
            continue;
        }
        if let Some(slot) = cache.iter().enumerate().find_map(|(slot, key)| {
            (slots[slot].is_none() && *key == Some(keys[index])).then_some(slot)
        }) {
            slots[slot] = Some(index);
        } else {
            pending.push(index);
        }
    }
    for index in pending {
        // The same head's previous map first; then empty storage rather than
        // evicting a currently dark emitter's map.
        let position = cones[index].position;
        let free = slots
            .iter()
            .enumerate()
            .find_map(|(slot, resident)| {
                (resident.is_none() && origins.get(slot).copied().flatten() == Some(position))
                    .then_some(slot)
            })
            .or_else(|| {
                slots.iter().enumerate().find_map(|(slot, resident)| {
                    (resident.is_none() && cache[slot].is_none()).then_some(slot)
                })
            })
            .or_else(|| slots.iter().position(Option::is_none));
        if let Some(slot) = free {
            slots[slot] = Some(index);
        }
    }
    slots
}

/// Which dirty maps to redraw this frame, at most `budget` of those that only
/// moved.
///
/// A dirty slot whose stored map was drawn from the same position (`moved`)
/// can keep showing that map for a few frames: it is the same head's shadow,
/// a little behind. Those compete for the budget by [`shadow_priority`] times
/// the frames they have already waited (`age`), so every one is redrawn within
/// a bounded number of frames. A slot with no map of its own head is always
/// drawn, because its stored map belongs to a different light.
pub(crate) fn budget_redraws(
    dirty: &[bool],
    moved: &[bool],
    priority: &[f32],
    age: &[u32],
    budget: usize,
) -> Vec<bool> {
    let mut ranked: Vec<usize> = (0..dirty.len())
        .filter(|&slot| dirty[slot] && moved[slot])
        .collect();
    let weight = |slot: usize| priority[slot].max(1e-6) * (1 + age[slot]) as f32;
    ranked.sort_by(|&a, &b| weight(b).total_cmp(&weight(a)).then(a.cmp(&b)));
    let mut redraw: Vec<bool> = (0..dirty.len())
        .map(|slot| dirty[slot] && !moved[slot])
        .collect();
    for slot in ranked.into_iter().take(budget) {
        redraw[slot] = true;
    }
    redraw
}

pub(crate) fn shadow_matrix_bits(matrix: &[[f32; 4]; 4]) -> [u32; 16] {
    let mut bits = [0; 16];
    for (target, value) in bits.iter_mut().zip(matrix.iter().flatten()) {
        *target = value.to_bits();
    }
    bits
}

pub(crate) fn fixture_shadow_caster_hash(frame: &Frame, opaque: usize) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut push_byte = |byte: u8| {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    };
    for draw in frame.draws.iter().take(opaque) {
        if matches!(
            &draw.editor_object,
            Some(crate::frame::EditorObject::Fixture(_))
        ) {
            continue;
        }
        for byte in frame.meshes[draw.mesh].key.as_bytes() {
            push_byte(*byte);
        }
        for value in draw.model.to_cols_array() {
            for byte in value.to_bits().to_le_bytes() {
                push_byte(byte);
            }
        }
    }
    hash
}

pub(crate) fn fixture_shadow_texture_array(
    device: &wgpu::Device,
    layers: u32,
) -> (wgpu::TextureView, Vec<wgpu::TextureView>) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fixture-shadow-atlas"),
        size: wgpu::Extent3d {
            width: FIXTURE_SHADOW_SIZE,
            height: FIXTURE_SHADOW_SIZE,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let array = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("fixture-shadow-atlas"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        base_array_layer: 0,
        array_layer_count: Some(layers),
        ..Default::default()
    });
    let render_layers = (0..layers)
        .map(|layer| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("fixture-shadow-layer"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                ..Default::default()
            })
        })
        .collect();
    (array, render_layers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FixtureCone;
    use glam::Vec3;

    /// The shadow map is the beam's own frustum: projected from the virtual
    /// apex, its near plane is the lens and its side planes pass through the
    /// lens rim. So the map cannot see the inside of the fixture, and it covers
    /// the whole lens-wide beam from the glass on.
    #[test]
    fn a_lens_shadow_frustum_starts_at_the_lens_and_is_lens_wide_there() {
        let half = 3f32.to_radians();
        let light = FixtureCone {
            position: Vec3::new(1.0, 2.0, 6.0),
            range: 20.0,
            direction: Vec3::NEG_Z,
            cos_beam: (half * 0.6).cos(),
            color: Vec3::ONE,
            intensity: 1.0,
            cos_field: half.cos(),
            wash: 0.0,
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
            lens: crate::luminaire::Lens { radius: 0.15 },
        };
        let lens = light.lens_distance();
        let (near, far) = fixture_shadow_planes(&light);
        assert_eq!(near, lens, "the near plane is the lens plane");
        assert!((far - (light.range + lens)).abs() < 1e-4);
        let matrix = fixture_shadow_matrix(&light);
        let ndc = |p: Vec3| matrix.project_point3(p);
        // Reverse-Z: the lens plane is depth 1, the far plane depth 0.
        assert!((ndc(light.position).z - 1.0).abs() < 1e-4);
        // The lens rim is the frustum edge at the near plane.
        let rim = ndc(light.position + Vec3::X * 0.15);
        assert!((rim.x.abs().max(rim.y.abs()) - 1.0).abs() < 1e-3, "{rim:?}");
        // Behind the lens plane is clipped away.
        assert!(ndc(light.position + Vec3::Z * 0.05).z > 1.0);
    }

    /// A point source keeps the planes and projection it always had.
    #[test]
    fn a_point_source_shadow_frustum_is_unchanged() {
        let light = FixtureCone {
            position: Vec3::new(0.0, 0.0, 4.0),
            range: 12.0,
            direction: Vec3::NEG_Z,
            cos_beam: 0.95,
            color: Vec3::ONE,
            intensity: 1.0,
            cos_field: 0.9,
            wash: 0.0,
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
            lens: crate::luminaire::Lens::POINT,
        };
        let (near, far) = fixture_shadow_planes(&light);
        assert!(near > 0.0 && near < far, "near {near} far {far}");
        assert_eq!(
            far, light.range,
            "a point source's frustum reaches its range"
        );
        assert_eq!(light.apex(), light.position);
    }

    /// Characterization, not a gate: today a cone that goes dark for one frame
    /// loses its shadow slot and comes back into a *different* one.
    ///
    /// This pins the cost behind the "0:49" report. Up to that point a real
    /// show's score lights 14 wash fixtures on one shared beat envelope: 14
    /// fits in [`MAX_FIXTURE_SHADOWS`], every cone holds a slot, the ranking
    /// never reorders, and no depth map is ever redrawn. Then one clip selects
    /// every fixture and modulates each separately — 30 candidates for 16
    /// slots. Slots are handed out by priority *rank*, and the shadow cache is
    /// keyed per slot by projection, so a cone that returns to a different slot
    /// dirties a depth map that never went stale.
    ///
    /// Asserting the churn rather than wishing it away, because the fix is a
    /// policy change (tenancy hysteresis over time, the temporal twin of
    /// `EVICTION_MARGIN`) that wants its own measurement — see
    /// `docs/design/volumetrics-v2.md`. When that lands, this test is the one
    /// that should start failing.
    /// Camera motion does NOT churn shadow tenancy — `EVICTION_MARGIN` is what
    /// stops it, and this pins that so a tuning change cannot quietly undo it.
    ///
    /// Priority is `intensity * radius^2 / distance^2`, so the camera *is* an
    /// input to which cones hold slots, and `1/d^2` is steep close in. The
    /// obvious reading — that zooming reorders the near cones and each
    /// reordering redraws a depth map — is wrong, and worth having written
    /// down: a challenger must beat the resident by 25% to take a slot, which a
    /// dolly of this size never manages at either distance. Whatever makes
    /// zooming expensive, it is not this.
    #[test]
    fn dollying_the_camera_does_not_reshuffle_shadow_tenancy() {
        // A depth-spread rig, so distance ordering is something the camera can
        // actually change — a wall of equidistant cones could not show this.
        let cones: Vec<_> = (0..MAX_FIXTURE_SHADOWS * 2)
            .map(|i| FixtureCone {
                position: Vec3::new((i % 8) as f32 - 4.0, (i / 8) as f32 * 3.0, 0.0),
                range: 5.0,
                direction: Vec3::Z,
                cos_beam: 0.98,
                color: Vec3::ONE,
                intensity: 1.0,
                cos_field: 0.95,
                wash: 0.0,
                gobo: 0,
                gobo_rotation: 0.0,
                haze_gain: 1.0,
                lens: crate::luminaire::Lens::POINT,
            })
            .collect();

        // One dolly step of the same size, taken from far away and from close in.
        let turnover = |from: f32, to: f32| {
            let settled = assign_shadow_slots(
                &cones,
                Vec3::new(0.0, from, 0.0),
                &[None; MAX_FIXTURE_SHADOWS],
            );
            let moved = assign_shadow_slots(&cones, Vec3::new(0.0, to, 0.0), &settled);
            settled
                .iter()
                .zip(&moved)
                .filter(|(before, after)| before != after)
                .count()
        };

        assert_eq!(
            (turnover(-60.0, -58.0), turnover(-8.0, -6.0)),
            (0, 0),
            "a 2m dolly must not move a shadow map, at either distance"
        );
    }

    #[test]
    fn a_cone_that_blinks_loses_its_shadow_slot_when_the_rig_is_over_the_cap() {
        let cone = |x: f32, intensity: f32| FixtureCone {
            position: Vec3::new(x, 0.0, 0.0),
            range: 5.0,
            direction: Vec3::Z,
            cos_beam: 0.98,
            color: Vec3::ONE,
            intensity,
            cos_field: 0.95,
            wash: 0.0,
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
            lens: crate::luminaire::Lens::POINT,
        };
        let eye = Vec3::new(0.0, -12.0, 0.0);

        // Under the cap every cone holds a slot, so blinking costs nothing:
        // this is the score's own "before 0:49" and it is already free.
        let few = MAX_FIXTURE_SHADOWS - 2;
        let lit: Vec<_> = (0..few).map(|i| cone(i as f32, 1.0)).collect();
        let blinked: Vec<_> = (0..few)
            .map(|i| cone(i as f32, if i % 3 == 0 { 0.0 } else { 1.0 }))
            .collect();
        let settled = assign_shadow_slots(&lit, eye, &[None; MAX_FIXTURE_SHADOWS]);
        let after = assign_shadow_slots(&lit, eye, &assign_shadow_slots(&blinked, eye, &settled));
        assert_eq!(
            settled, after,
            "under the cap, a blink must not move anything"
        );

        // Over the cap it does move: the dark cones' slots are taken by lit
        // challengers that were previously unshadowed, and the originals come
        // back somewhere else — or not at all.
        let many = MAX_FIXTURE_SHADOWS * 2;
        let lit: Vec<_> = (0..many).map(|i| cone(i as f32, 1.0)).collect();
        let blinked: Vec<_> = (0..many)
            .map(|i| cone(i as f32, if i % 3 == 0 { 0.0 } else { 1.0 }))
            .collect();
        let settled = assign_shadow_slots(&lit, eye, &[None; MAX_FIXTURE_SHADOWS]);
        let after = assign_shadow_slots(&lit, eye, &assign_shadow_slots(&blinked, eye, &settled));
        assert_ne!(
            settled, after,
            "over the cap, one blink is expected to reshuffle tenancy today"
        );
        let moved = settled
            .iter()
            .zip(&after)
            .filter(|(before, now)| before != now)
            .count();
        assert!(
            moved > 0,
            "the reshuffle should be visible as slots changing tenant"
        );
    }

    fn spot(position: Vec3, direction: Vec3) -> FixtureCone {
        FixtureCone {
            position,
            range: 20.0,
            direction,
            cos_beam: 0.95,
            color: Vec3::ONE,
            intensity: 1.0,
            cos_field: 0.9,
            wash: 0.0,
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
            lens: crate::luminaire::Lens::POINT,
        }
    }

    /// A head that pans returns to the slot its previous map is in, even when
    /// another slot is empty, so that map can stand in until it is redrawn.
    #[test]
    fn a_moving_head_keeps_its_own_slot() {
        let first = vec![
            spot(Vec3::new(-2.0, 0.0, 6.0), Vec3::NEG_Z),
            spot(Vec3::new(2.0, 0.0, 6.0), Vec3::NEG_Z),
        ];
        let mut cache = vec![None; 4];
        let mut origins = vec![None; 4];
        let slots = assign_cached_slots(&first, &cache, &origins, 7);
        for (slot, resident) in slots.iter().enumerate() {
            if let Some(index) = resident {
                cache[slot] = Some(ShadowCacheKey {
                    matrix_bits: shadow_matrix_bits(
                        &fixture_shadow_matrix(&first[*index]).to_cols_array_2d(),
                    ),
                    caster_hash: 7,
                });
                origins[slot] = Some(first[*index].position);
            }
        }
        // Reverse the list order too: identity is the position, not the index.
        let panned = vec![
            spot(
                Vec3::new(2.0, 0.0, 6.0),
                Vec3::new(0.3, 0.0, -1.0).normalize(),
            ),
            spot(
                Vec3::new(-2.0, 0.0, 6.0),
                Vec3::new(-0.3, 0.0, -1.0).normalize(),
            ),
        ];
        let moved = assign_cached_slots(&panned, &cache, &origins, 7);
        for (slot, resident) in moved.iter().enumerate() {
            if let Some(index) = resident {
                assert_eq!(origins[slot], Some(panned[*index].position));
            }
        }
    }

    #[test]
    fn redraw_budget_defers_only_moved_maps_and_ages_them_in() {
        let dirty = [true, true, true, true, false];
        let moved = [false, true, true, true, true];
        let priority = [0.1, 1.0, 5.0, 2.0, 9.0];
        let redraw = budget_redraws(&dirty, &moved, &priority, &[0; 5], 1);
        // A fresh map always draws; one moved map fits, the brightest.
        assert_eq!(redraw, vec![true, false, true, false, false]);
        // Waiting raises a dim map above a bright one that just drew.
        let redraw = budget_redraws(&dirty, &moved, &priority, &[0, 9, 0, 0, 0], 1);
        assert_eq!(redraw, vec![true, true, false, false, false]);
        // An unlimited budget draws every dirty map.
        let all = budget_redraws(&dirty, &moved, &priority, &[0; 5], usize::MAX);
        assert_eq!(all, dirty.to_vec());
    }
}
