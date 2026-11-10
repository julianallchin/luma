//! A resize drag keeps the targets, grids and caches it can, and reuses
//! memory for the rest (`target_memory.rs`). None of that may show: a stage
//! dragged to a size draws what a stage that stayed at that size draws.

use super::{fixture_surface_frame, Renderer};

/// Mean absolute difference per channel, in 8-bit steps.
fn mean_difference(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    total as f64 / a.len() as f64
}

/// Drag one renderer through a size a frame, growing then shrinking. At
/// every sixth size, compare it with a renderer that drew the same number of
/// frames at that size alone: the renderer converges some state over frames
/// (probe captures, a probe a frame), so only the sizes may differ.
fn dragged_against_still(haze: f32, live: bool) -> anyhow::Result<Vec<f64>> {
    let draw = |renderer: &mut Renderer, frame, width, height| {
        if live {
            renderer.render_next(frame, width, height, crate::LIVE_SUBFRAMES)
        } else {
            renderer.render(frame, width, height, 1)
        }
    };
    let mut frame = fixture_surface_frame(3);
    frame.geometry_shadows = true;
    frame.haze_density = haze;
    frame.haze_resolution = 1.0;
    let sizes: Vec<(u32, u32)> = (0..24)
        .map(|step| (96 + 2 * step, 72 + step))
        .chain((0..24).rev().map(|step| (96 + 3 * step, 72 + step)))
        .collect();
    let mut dragged = Renderer::new()?;
    let mut differences = Vec::new();
    for (index, &(width, height)) in sizes.iter().enumerate() {
        let pixels = draw(&mut dragged, &frame, width, height)?;
        if index % 6 == 5 {
            let mut still = Renderer::new()?;
            let mut expected = Vec::new();
            for _ in 0..=index {
                expected = draw(&mut still, &frame, width, height)?;
            }
            differences.push(mean_difference(&pixels, &expected));
        }
    }
    Ok(differences)
}

#[test]
fn a_dragged_stage_draws_what_a_still_one_draws() -> anyhow::Result<()> {
    for live in [false, true] {
        for difference in dragged_against_still(0.0, live)? {
            assert_eq!(
                difference, 0.0,
                "a resized frame differs from a still one (live: {live})"
            );
        }
    }
    Ok(())
}

/// The haze path keeps its froxel grid, light index and lit-interval cache
/// across sizes where it can (`fog_grid.rs`, `light_index.rs`,
/// `haze_cache.rs`) and places its volumes uncleared; none of it may show.
///
/// Not exactly zero: a hazy renderer that has changed size already differed
/// from a still one by a few low bits in a handful of pixels (about 1e-4 of a
/// step on average) before any of that. Reused memory that leaked into the
/// picture would cost whole steps.
#[test]
fn a_dragged_hazy_stage_draws_what_a_still_one_draws() -> anyhow::Result<()> {
    for live in [false, true] {
        for difference in dragged_against_still(0.6, live)? {
            assert!(
                difference < 0.01,
                "a resized hazy frame differs by {difference} (live: {live})"
            );
        }
    }
    Ok(())
}
