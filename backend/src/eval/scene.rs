//! The unified render surface: one [`Scene`] of compiled annotations, one
//! [`Scene::render`] entry point that serves realtime (one frame), editor scrub
//! (one frame at the playhead), and batch/export (a dense time axis) from the
//! same code — because every op is a pure function of absolute time, any frame
//! can be the first frame computed, with no warmup.
//!
//! [`Scope`] picks *what* to render: a single annotation in isolation (pattern
//! preview / graph editor) or the z-ordered composite of all active annotations
//! (track playback / perform). Compositing is the [`composite`](super::composite)
//! fold, not an IR op — see that module for why.
//!
//! Rate-free by construction: `times` is an arbitrary `&[f32]`. The 44 Hz DMX
//! tick and the frame-cache grain live at the *output* boundary (the render
//! loop / emitter), never here.

use crate::eval::composite::{blank_frame, composite_frame};
use crate::eval::{try_eval, Arena, BlendMode, Plan};
use crate::models::universe::UniverseState;
use std::sync::Arc;

/// One placed, compiled pattern on the timeline: its evaluable [`Plan`] plus the
/// timeline metadata the compositor needs (when it's active, where it sits in
/// z-order, how it blends). The plan also carries `span` in its `ctx`; the
/// explicit copy here is what the active-set query reads.
#[derive(Clone, Debug)]
pub struct CompiledAnnotation {
    /// `Arc` so an unchanged annotation's plan is reused (incremental composite)
    /// and the preview generator can share it — both are O(1) clones.
    pub plan: Arc<Plan>,
    /// Absolute `[start, end)` the annotation is active over.
    pub span: (f32, f32),
    pub z_index: i64,
    pub blend_mode: BlendMode,
}

/// A compiled, evaluable lighting program for one `(track, venue)` — every
/// annotation's plan, ready to render either singly or composited.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    /// Annotations in z-order ascending (painter's algorithm: lower z first).
    pub annotations: Vec<CompiledAnnotation>,
}

/// What to render from a [`Scene`].
#[derive(Clone, Copy, Debug)]
pub enum Scope {
    /// One annotation in isolation, no compositing, no span mask — the raw
    /// pattern output (preview cards, graph editor live view).
    Single(usize),
    /// The z-ordered composite of all annotations active at each time.
    Composite,
}

impl Scene {
    /// Build from annotations in any order; sorts to z-ascending so
    /// [`Scope::Single`] indices are stable and compositing is painter-ordered.
    pub fn new(mut annotations: Vec<CompiledAnnotation>) -> Self {
        annotations.sort_by_key(|a| a.z_index);
        Self { annotations }
    }

    pub fn is_empty(&self) -> bool {
        self.annotations.is_empty()
    }

    /// Evaluate over `times`, one [`UniverseState`] per sample. `times.len() == 1`
    /// is a realtime / scrub frame; a dense grid is a bake.
    pub fn render(&self, times: &[f32], scope: Scope, scratch: &mut Arena) -> Vec<UniverseState> {
        self.try_render(times, scope, scratch)
            .unwrap_or_else(|error| {
                log::error!("Graph evaluation: {error}");
                times.iter().map(|_| blank_frame()).collect()
            })
    }

    pub fn try_render(
        &self,
        times: &[f32],
        scope: Scope,
        scratch: &mut Arena,
    ) -> Result<Vec<UniverseState>, String> {
        match scope {
            // Raw single-pattern output — exactly what the per-pattern goldens
            // validated. No span mask: the caller chose the times.
            Scope::Single(idx) => match self.annotations.get(idx) {
                Some(ann) => try_eval(ann.plan.as_ref(), times, scratch),
                None => Ok(times.iter().map(|_| blank_frame()).collect()),
            },
            Scope::Composite => self.composite(times, scratch),
        }
    }

    /// Each annotation evaluates one batch of its active times. Samples outside
    /// its clip must neither contribute output nor cause an evaluation failure.
    fn composite(&self, times: &[f32], scratch: &mut Arena) -> Result<Vec<UniverseState>, String> {
        let mut frames: Vec<UniverseState> = times.iter().map(|_| blank_frame()).collect();
        for ann in &self.annotations {
            let active = |t: &f32| *t >= ann.span.0 && *t < ann.span.1;
            if !times.iter().any(active) {
                continue;
            }
            let sample_times: std::borrow::Cow<'_, [f32]> = if times.iter().all(active) {
                times.into()
            } else {
                times
                    .iter()
                    .copied()
                    .filter(active)
                    .collect::<Vec<_>>()
                    .into()
            };
            let got = try_eval(ann.plan.as_ref(), &sample_times, scratch)?;
            for ((k, _), frame) in times.iter().enumerate().filter(|(_, t)| active(t)).zip(got) {
                composite_frame(
                    &mut frames[k],
                    &frame,
                    &ann.plan.outputs,
                    ann.blend_mode,
                    1.0,
                    None,
                );
            }
        }
        Ok(frames)
    }
}
