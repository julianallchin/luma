//! Haze state carried between frames: the shared fog grid, the lit-interval
//! cache and the residual compaction.

use std::sync::Arc;

use glam::{Mat4, Vec3};

use crate::frame::{Draw, MaterialTextures};
use luma_scene::mesh::Material;

use super::fixture_surface_frame;
use crate::gpu::{Gpu, Renderer};

#[test]
fn shared_fog_visibility_updates_after_a_caster_moves() -> anyhow::Result<()> {
    let mut frame = fixture_surface_frame(1);
    let mut light = frame.fixture_cones[0];
    light.position = Vec3::new(0.0, 0.0, 8.0);
    light.range = 24.0;
    light.wash = 0.9;
    light.cos_field = 0.6;
    light.cos_beam = 0.85;
    light.intensity = 1.0;
    let mut decoy = light;
    decoy.direction = Vec3::Z;
    frame.fixture_cones = vec![decoy; 300];
    frame.fixture_cones.push(light);
    frame.geometry_shadows = true;
    frame.fixture_surface_lighting = false;
    frame.haze_density = 0.8;
    frame.haze_steps = 8;
    frame.camera.eye = Vec3::new(0.0, -12.0, 6.0);
    frame.draws.push(Draw {
        mesh: 0,
        model: Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0))
            * Mat4::from_scale(Vec3::splat(0.25)),
        material: Material::default(),
        textures: MaterialTextures::default(),
        editor_object: None,
    });
    let mut renderer = Renderer::new()?;
    let blocked = renderer.render(&frame, 160, 120, 1)?;
    assert_eq!(
        renderer
            .targets
            .as_ref()
            .unwrap()
            .fog
            .integral
            .texture()
            .depth_or_array_layers(),
        crate::fog_grid::SLICES + 1
    );
    frame.draws[1].model = Mat4::from_translation(Vec3::new(30.0, 0.0, 5.0));
    let open = renderer.render(&frame, 160, 120, 1)?;
    let energy = |p: &[u8]| {
        p.chunks_exact(4)
            .map(|p| p[..3].iter().map(|v| u64::from(*v)).sum::<u64>())
            .sum::<u64>()
    };
    assert!(
        energy(&blocked) < energy(&open),
        "the shared grid retained an obsolete caster shadow"
    );
    Ok(())
}

#[test]
fn shared_washes_preserve_colour_energy_and_reset_on_cues() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    // One much brighter emitter and mixed colours challenge the shared
    // grid and quad sharing against the per-ray reference.
    let mut frame = fixture_surface_frame(137);
    frame.geometry_shadows = true;
    frame.haze_density = 0.7;
    frame.haze_steps = 8;
    frame.fixture_surface_lighting = false;
    for (i, light) in frame.fixture_cones.iter_mut().enumerate() {
        light.wash = 0.8;
        light.cos_field = 0.6;
        light.cos_beam = 0.85;
        light.color = if i % 3 == 0 {
            Vec3::X
        } else {
            Vec3::new(0.02, 0.3, 1.0)
        };
    }
    frame.fixture_cones[136].intensity *= 80.0;
    let reference = renderer.render_reference(&frame, 160, 120, 32)?;
    let shared = renderer.render(&frame, 160, 120, 1)?;
    let energy = |p: &[u8], channel: usize| {
        p.chunks_exact(4)
            .map(|p| f64::from(p[channel]))
            .sum::<f64>()
    };
    for channel in 0..3 {
        let expected = energy(&reference, channel);
        let actual = energy(&shared, channel);
        assert!(expected > 1000.0, "test must contain visible fog");
        assert!(
            (actual - expected).abs() / expected < 0.05,
            "sharing changed channel {channel} energy: {actual} vs {expected}"
        );
    }
    for i in 0..8 {
        frame.time = i as f32 / 60.0;
        renderer.render_next(&frame, 160, 120, 1)?;
    }
    for light in &mut frame.fixture_cones {
        light.color = Vec3::Y;
        light.intensity *= 0.05;
    }
    frame.time += 1.0 / 60.0;
    let cue = renderer.render_next(&frame, 160, 120, 1)?;
    let fresh = renderer.render(&frame, 160, 120, 1)?;
    assert_eq!(cue, fresh, "a colour/dimmer cue retained stale fog history");
    Ok(())
}

#[test]
fn interval_cache_and_compact_list_resume_after_empty_blackout() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let mut frame = fixture_surface_frame(300);
    frame.geometry_shadows = true;
    frame.fixture_surface_lighting = false;
    frame.haze_density = 0.3;
    frame.haze_resolution = 1.0;

    for _ in 0..3 {
        renderer.render(&frame, 80, 60, 1)?;
    }
    assert_eq!(renderer.interval_cache_stats().read, 300);
    assert!(renderer.compact_stats().active);
    assert_eq!(renderer.compact_stats().dirty_slots, 0);

    let lights = std::mem::take(&mut frame.fixture_cones);
    renderer.render(&frame, 80, 60, 1)?;
    assert_eq!(
        renderer.interval_cache_stats(),
        crate::interval_cache::IntervalCacheStats::default()
    );
    assert!(!renderer.compact_stats().active);

    frame.fixture_cones = lights;
    renderer.render(&frame, 80, 60, 1)?;
    assert_eq!(renderer.shadow_stats().redrawn_maps, 0);
    assert_eq!(renderer.interval_cache_stats().read, 300);
    assert!(renderer.compact_stats().active);
    assert_eq!(renderer.compact_stats().dirty_slots, 0);
    Ok(())
}

#[test]
fn partial_blackout_retains_only_resident_interval_slots() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let mut frame = fixture_surface_frame(300);
    frame.geometry_shadows = true;
    frame.fixture_surface_lighting = false;
    frame.haze_density = 0.3;
    frame.haze_resolution = 1.0;
    for _ in 0..3 {
        renderer.render(&frame, 80, 60, 1)?;
    }

    let lights = frame.fixture_cones.clone();
    frame.fixture_cones.truncate(3);
    renderer.render(&frame, 80, 60, 1)?;
    assert_eq!(renderer.interval_cache_stats().read, 3);

    frame.fixture_cones.clear();
    renderer.render(&frame, 80, 60, 1)?;
    assert_eq!(
        renderer.interval_cache_stats(),
        crate::interval_cache::IntervalCacheStats::default()
    );

    frame.fixture_cones = lights;
    renderer.render(&frame, 80, 60, 1)?;
    assert_eq!(renderer.shadow_stats().redrawn_maps, 0);
    assert_eq!(renderer.interval_cache_stats().read, 3);
    assert_eq!(renderer.interval_cache_stats().write, 297);
    assert!(renderer.compact_stats().active);
    assert_eq!(renderer.compact_stats().dirty_slots, 300);
    Ok(())
}

#[test]
fn residual_compaction_encodes_more_than_256_resident_lights() -> anyhow::Result<()> {
    // A payload table of 256 four-way buckets cannot hold every pair, so
    // the lost entries exercise the exact-traversal segment on any
    // adapter rather than only where the interval counts happen to
    // overflow `CACHE_K`.
    let mut gpu = Gpu::build()?;
    gpu.interval_cache_table_bits = 10;
    let mut renderer = Renderer::on(Arc::new(gpu));
    renderer.compaction.group_columns = 2;
    let light_count = 300;
    let high_start = 256;
    let mut frame = fixture_surface_frame(light_count);
    frame.geometry_shadows = true;
    frame.fixture_surface_lighting = false;
    frame.haze_density = 0.3;
    frame.haze_resolution = 1.0;
    for light in &mut frame.fixture_cones {
        light.intensity = 0.001;
    }

    // Warm the interval cache, then keep settled frames on the fused
    // path. The following frame resumes compaction in the same renderer,
    // so the captures differ only by that path and the repeated fused
    // frame proves that the test scene itself is deterministic.
    renderer.compaction.suspended = 5;
    renderer.render(&frame, 96, 72, 1)?;
    assert_eq!(renderer.shadowed_fixture_count(), light_count);
    let high_lights: Vec<_> = renderer.fixture_shadow_slots[high_start..light_count]
        .iter()
        .map(|light| light.expect("resident high slot"))
        .collect();
    for (index, light) in high_lights.iter().copied().enumerate() {
        frame.fixture_cones[light].color = [Vec3::X, Vec3::Y, Vec3::Z][index % 3];
        frame.fixture_cones[light].intensity = 0.01;
    }
    renderer.render(&frame, 96, 72, 1)?;
    assert!(!renderer.compact_stats().active);
    renderer.render(&frame, 96, 72, 1)?;
    assert!(!renderer.compact_stats().active);
    let fused = renderer.render(&frame, 96, 72, 1)?;
    assert!(!renderer.compact_stats().active);
    let fused_repeat = renderer.render(&frame, 96, 72, 1)?;
    assert!(!renderer.compact_stats().active);
    let fused_floor = fused
        .iter()
        .zip(&fused_repeat)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert_eq!(fused_floor, 0, "settled fused frame was not stable");
    for (slot, light) in high_lights.iter().copied().enumerate() {
        assert_eq!(
            renderer.fixture_shadow_slots[high_start + slot],
            Some(light)
        );
    }

    let compact = renderer.render(&frame, 96, 72, 1)?;
    let stats = renderer.compact_stats();
    assert!(stats.active);
    assert_eq!(stats.dense_slots, light_count as u32);
    let max_delta = fused
        .iter()
        .zip(&compact)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    let mut changed_pixels = 0usize;
    let mut sum_squared = 0u64;
    let mut max_location = 0usize;
    for (pixel, (a, b)) in fused
        .chunks_exact(4)
        .zip(compact.chunks_exact(4))
        .enumerate()
    {
        if a != b {
            changed_pixels += 1;
        }
        for channel in 0..4 {
            let delta = a[channel].abs_diff(b[channel]);
            sum_squared += u64::from(delta) * u64::from(delta);
            if delta == max_delta {
                max_location = pixel * 4 + channel;
            }
        }
    }
    let rmse = (sum_squared as f64 / fused.len() as f64).sqrt();
    assert!(
        max_delta <= 1,
        "compaction changed {changed_pixels} pixels, max {max_delta} at byte {max_location}, RMSE {rmse:.4}; settled fused repeat floor max {fused_floor}"
    );
    renderer.render(&frame, 96, 72, 1)?;
    let completed = renderer.compact_stats();
    assert_eq!(completed.segments.iter().sum::<u32>(), completed.list_count);
    assert!(completed.segments.iter().all(|count| *count > 0));
    assert!(
        completed
            .segments
            .into_iter()
            .zip(crate::haze_compact::RESID_LANES)
            .any(|(count, lanes)| count > lanes * renderer.compaction.group_columns),
        "no segment crossed an artificial dispatch row: {completed:?}"
    );
    Ok(())
}

#[test]
fn residual_overflow_falls_back_on_its_first_frame() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    renderer.compaction.capacity = 64;
    let mut frame = fixture_surface_frame(300);
    frame.geometry_shadows = true;
    frame.fixture_surface_lighting = false;
    frame.haze_density = 0.3;
    frame.haze_resolution = 1.0;

    // Warm a stable fused oracle in this renderer without allocating
    // compact pools. The odd dimensions exercise partial edge groups.
    renderer.compaction.suspended = 6;
    for _ in 0..4 {
        renderer.render(&frame, 97, 73, 1)?;
    }
    let fused = renderer.render(&frame, 97, 73, 1)?;
    renderer.compaction.suspended = 0;

    let first = renderer.render(&frame, 97, 73, 1)?;
    assert!(renderer.compact_stats().active);
    let max_delta = fused
        .iter()
        .zip(&first)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert!(
        max_delta <= 1,
        "first overflow frame did not use the fused oracle (max delta {max_delta})"
    );

    for _ in 0..3 {
        renderer.render(&frame, 97, 73, 1)?;
        if renderer.compact_stats().overflow {
            break;
        }
    }
    let completed = renderer.compact_stats();
    assert!(
        completed.overflow,
        "overflow readback did not land: {completed:?}"
    );
    assert!(
        completed.list_count > 64,
        "total overflow was not exercised"
    );
    Ok(())
}

#[test]
fn shared_residual_queue_repacks_incremental_and_gc_lists() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let mut frame = fixture_surface_frame(300);
    frame.geometry_shadows = true;
    frame.fixture_surface_lighting = false;
    frame.haze_density = 0.3;
    frame.haze_resolution = 1.0;
    for _ in 0..5 {
        renderer.render(&frame, 80, 60, 1)?;
    }
    let baseline = renderer.compact_stats();
    assert!(baseline.list_count > 0);
    assert_eq!(baseline.segments.iter().sum::<u32>(), baseline.list_count);

    frame.fixture_cones[0].position.x += 0.01;
    renderer.render(&frame, 80, 60, 1)?;
    let planned = renderer.compact_stats();
    assert!(planned.classify);
    assert!(!planned.rebuilt);
    assert_eq!(planned.dirty_slots, 1);
    let readbacks = planned.readbacks;
    for _ in 0..4 {
        renderer.render(&frame, 80, 60, 1)?;
        if renderer.compact_stats().readbacks > readbacks {
            break;
        }
    }
    let appended = renderer.compact_stats();
    assert!(appended.list_count > baseline.list_count);
    assert_eq!(appended.segments.iter().sum::<u32>(), appended.list_count);

    renderer.compaction.full_count = 1;
    renderer.render(&frame, 80, 60, 1)?;
    let planned = renderer.compact_stats();
    assert!(planned.classify);
    assert!(planned.rebuilt);
    let readbacks = planned.readbacks;
    for _ in 0..4 {
        renderer.render(&frame, 80, 60, 1)?;
        if renderer.compact_stats().readbacks > readbacks {
            break;
        }
    }
    let rebuilt = renderer.compact_stats();
    assert!(rebuilt.list_count < appended.list_count);
    assert_eq!(rebuilt.segments.iter().sum::<u32>(), rebuilt.list_count);
    Ok(())
}

#[test]
fn compact_output_dispatch_tracks_equal_area_reshape() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let mut frame = fixture_surface_frame(300);
    frame.geometry_shadows = true;
    frame.fixture_surface_lighting = false;
    frame.haze_density = 0.3;
    frame.haze_resolution = 1.0;
    for _ in 0..3 {
        renderer.render(&frame, 96, 72, 1)?;
    }
    assert!(renderer.compact_stats().active);

    // Both sizes have 216 haze workgroups. Let the reshaped frame key
    // settle, then compare its compact READ frame with a forced fused
    // frame at the same size. The rebuild must refresh the indirect x/y
    // dimensions even though the total workgroup count did not change.
    renderer.render(&frame, 72, 96, 1)?;
    assert!(!renderer.compact_stats().active);
    renderer.render(&frame, 72, 96, 1)?;
    assert!(renderer.compact_stats().active);
    assert!(renderer.compact_stats().rebuilt);
    let compact = renderer.render(&frame, 72, 96, 1)?;
    assert!(renderer.compact_stats().active);
    renderer.compaction.suspended = 5;
    for _ in 0..3 {
        renderer.render(&frame, 72, 96, 1)?;
    }
    let fused = renderer.render(&frame, 72, 96, 1)?;
    assert!(!renderer.compact_stats().active);
    let fused_repeat = renderer.render(&frame, 72, 96, 1)?;
    assert!(!renderer.compact_stats().active);
    let fused_floor = fused
        .iter()
        .zip(&fused_repeat)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert_eq!(fused_floor, 0, "settled fused reshape frame was not stable");
    let max_delta = fused
        .iter()
        .zip(&compact)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert!(
        max_delta <= 1,
        "reshape left stale output groups (max delta {max_delta})"
    );
    Ok(())
}

/// The interval cache, its write generations and the compaction's
/// per-slot classification carry state from frame to frame. Through
/// transitions (a moved occluder, a resize, haze off, a camera move, a gobo
/// toggle, a dark cone, shadows off) a settled frame must equal what the
/// same renderer settles to from cold haze caches, bit for bit. The
/// unsettled steps in between render once each, so state such as a
/// classification kept across a resize meets the frames that reuse it.
#[test]
fn haze_state_survives_frame_transitions() -> anyhow::Result<()> {
    let mut frame = fixture_surface_frame(3);
    frame.geometry_shadows = true;
    frame.haze_density = 0.6;
    frame.haze_resolution = 1.0;
    frame.haze_steps = 8;
    frame.fixture_cones[0].wash = 1.0;
    frame.draws.push(Draw {
        mesh: 0,
        model: Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0))
            * Mat4::from_scale(Vec3::splat(0.07)),
        material: Material::default(),
        textures: MaterialTextures::default(),
        editor_object: None,
    });
    // The reference sees the same frames, so state outside the haze
    // (shadow-slot residency, cached maps) has the same history; only its
    // haze caches start cold at every probe.
    let gpu = Arc::new(Gpu::build()?);
    let mut renderer = Renderer::on(gpu.clone());
    let mut reference = Renderer::on(gpu);
    let mut first = Vec::new();
    let original_intensity = frame.fixture_cones[1].intensity;
    for (probe, (width, height), settle) in [
        (0, (127, 97), true),
        (1, (127, 97), false),
        (2, (127, 97), false),
        (3, (65, 49), false),
        (4, (127, 97), false),
        (5, (127, 97), false),
        (6, (127, 97), false),
        (7, (127, 97), true),
        (8, (127, 97), true),
        (9, (127, 97), false),
        (10, (127, 97), true),
        (11, (127, 97), true),
    ] {
        match probe {
            2 => frame.draws[1].model = Mat4::from_translation(Vec3::new(20.0, 0.0, 2.0)),
            4 => frame.haze_density = 0.0,
            5 => {
                frame.haze_density = 0.6;
                frame.camera.eye.x += 0.2;
            }
            6 => frame.fixture_cones[1].gobo = 1,
            7 => frame.fixture_cones[1].gobo = 0,
            8 => frame.fixture_cones[1].intensity = 0.0,
            9 => frame.fixture_shadows = false,
            10 => {
                frame.fixture_shadows = true;
                frame.fixture_cones[1].intensity = original_intensity;
            }
            11 => {
                renderer.visibility_reference = true;
                reference.visibility_reference = true;
            }
            _ => {}
        }
        if !settle {
            let actual = renderer.render_next(&frame, width, height, crate::LIVE_SUBFRAMES)?;
            reference.render_next(&frame, width, height, crate::LIVE_SUBFRAMES)?;
            if probe == 4 {
                assert_ne!(
                    actual, first,
                    "haze-off transition did not change the image"
                );
            }
            continue;
        }
        // Cold, write, read: three frames settle a fresh cache.
        reference.forget_haze_history();
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for _ in 0..3 {
            actual = renderer.render_next(&frame, width, height, crate::LIVE_SUBFRAMES)?;
            expected = reference.render_next(&frame, width, height, crate::LIVE_SUBFRAMES)?;
        }
        if actual != expected {
            let pixels = actual
                .chunks_exact(4)
                .zip(expected.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count();
            let max = actual
                .iter()
                .zip(&expected)
                .map(|(a, b)| a.abs_diff(*b))
                .max();
            panic!("probe {probe}: {pixels} pixels differ from a cold haze cache, max {max:?}");
        }
        if probe == 0 {
            first = actual;
        }
    }
    Ok(())
}

#[test]
fn fog_block_stats_follow_frames_and_resizes() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    assert!(renderer.fog_block_stats()?.is_none());
    let mut frame = fixture_surface_frame(3);
    frame.geometry_shadows = true;
    frame.haze_density = 0.6;
    for light in &mut frame.fixture_cones {
        light.wash = 1.0;
        light.range = 15.0;
        light.cos_field = 0.0;
        light.cos_beam = 0.1;
    }
    renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
    let first = renderer.fog_block_stats()?.expect("classified frame");
    assert_eq!(first.image_size, [127, 97]);
    let tile = crate::fog_grid::TILE_SIZE;
    let first_grid = [127_u32.div_ceil(tile), 97_u32.div_ceil(tile), 128];
    let first_blocks = first_grid.map(|n| n.div_ceil(4));
    assert_eq!(first.grid_size, first_grid);
    assert_eq!(first.blocks, first_blocks);
    assert_eq!(
        first.counts.len(),
        first_blocks.into_iter().product::<u32>() as usize
    );
    assert!(first.counts.iter().any(|counts| counts[0] > 0));
    assert!(first
        .counts
        .iter()
        .all(|counts| counts[1] <= counts[0] && counts[0] <= 3));
    assert_eq!(renderer.fog_block_stats()?.unwrap().counts, first.counts);
    renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
    assert_eq!(renderer.fog_block_stats()?.unwrap().counts, first.counts);
    // The lighting shader trusts these source eligibility filters. A cue
    // must clear candidates that become ineligible, then restore them.
    frame.fixture_cones[0].wash = crate::fog_grid::BROAD_WASH - 0.01;
    frame.fixture_cones[1].gobo = 1;
    frame.fixture_cones[2].haze_gain = 0.0;
    renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
    assert!(renderer
        .fog_block_stats()?
        .unwrap()
        .counts
        .iter()
        .all(|counts| *counts == [0, 0]));
    frame.fixture_cones[0].wash = 1.0;
    frame.fixture_cones[1].gobo = 0;
    frame.fixture_cones[2].haze_gain = 1.0;
    renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
    assert_eq!(renderer.fog_block_stats()?.unwrap().counts, first.counts);
    renderer.render_next(&frame, 65, 49, crate::LIVE_SUBFRAMES)?;
    let resized = renderer.fog_block_stats()?.expect("resized frame");
    let resized_grid = [65_u32.div_ceil(tile), 49_u32.div_ceil(tile), 128];
    let resized_blocks = resized_grid.map(|n| n.div_ceil(4));
    assert_eq!(resized.grid_size, resized_grid);
    assert_eq!(resized.blocks, resized_blocks);
    assert_eq!(
        resized.counts.len(),
        resized_blocks.into_iter().product::<u32>() as usize
    );
    frame.haze_density = 0.0;
    renderer.render_next(&frame, 65, 49, crate::LIVE_SUBFRAMES)?;
    assert!(renderer.fog_block_stats()?.is_none());
    Ok(())
}
