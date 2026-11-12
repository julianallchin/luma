//! Render Engine
//!
//! Owns all rendering state (layers, universe generation, DMX output).
//! Decoupled from audio playback — reads time from HostAudioState only in
//! edit mode. In perform mode it renders per-deck layers and blends by volume.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::eval::{Arena, Scene, Scope};
use crate::models::universe::{PrimitiveState, UniverseState};
use crate::perform::clock::DeckClock;

/// Sample a [`Scene`] at a single absolute time → one [`UniverseState`]. The
/// realtime collapse of the unified `render` API (the render loop's hot path).
#[inline]
fn sample_scene(scene: &Scene, t: f32, scratch: &mut Arena) -> UniverseState {
    scene
        .render(&[t], Scope::Composite, scratch)
        .pop()
        .unwrap_or_default()
}

/// One followed deck, as live follow hands it to the engine.
#[derive(Clone, Copy, Debug)]
pub struct PerformDeckInput {
    pub deck_id: u8,
    /// Extrapolated at every sample, so the lights move between reports.
    pub clock: DeckClock,
    /// Blend weight. Zero drops the deck from the mix.
    pub volume: f32,
}

// ============================================================================
// RenderEngine
// ============================================================================

#[derive(Clone)]
pub struct RenderEngine {
    inner: Arc<Mutex<RenderEngineInner>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SceneTarget {
    Active,
    Deck(u8),
    /// The Perform set.
    Live,
}

use crate::perform::live::{LiveClock, LiveSet};

pub(crate) struct SceneUpdate {
    target: SceneTarget,
    generation: u64,
}

impl SceneUpdate {
    /// This update's number, which no other update of the engine has.
    pub(crate) fn id(&self) -> u64 {
        self.generation
    }
}

/// What a finished [`SceneUpdate`] installs: a compiled scene for the editor
/// or a deck, or the Perform set. An empty scene or `None` clears the slot.
pub(crate) enum Built {
    Scene(Scene),
    Live(Option<LiveSet>),
}

pub(crate) struct RenderEngineInner {
    scene_generation: u64,
    pending_scenes: HashMap<SceneTarget, u64>,
    /// Active scene for track editor / pattern editor (composited per frame).
    active_scene: Option<Scene>,
    /// The score the active slot was last asked to show, installed or still
    /// compiling. Leaving a score clears the slot only while it is this one.
    active_score: Option<String>,
    /// Per-deck scenes for perform mode (the track's full composite per deck).
    perform_layers: HashMap<u8, Scene>,
    /// Per-deck clocks and blend weights, set by live follow.
    perform_deck_states: Vec<PerformDeckInput>,
    /// Seconds added to every deck's position: positive lights lead the music.
    perform_latency: f32,
    /// The Perform set, over the decks or the editor.
    live: Option<LiveSet>,
    /// Reusable eval scratch arena, held across frames so the hot path stays warm.
    scratch: Arena,
}

impl Default for RenderEngine {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RenderEngineInner {
                scene_generation: 0,
                pending_scenes: HashMap::new(),
                active_scene: None,
                active_score: None,
                perform_layers: HashMap::new(),
                perform_deck_states: Vec::new(),
                perform_latency: 0.0,
                live: None,
                scratch: Arena::default(),
            })),
        }
    }
}

impl RenderEngine {
    pub fn reset_for_identity_switch(&self) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.active_scene = None;
        guard.active_score = None;
        guard.pending_scenes.clear();
        guard.perform_layers.clear();
        guard.perform_deck_states.clear();
        guard.live = None;
        guard.scratch = Arena::default();
    }

    /// The installed track scene at one absolute time, or `None` when nothing
    /// is installed.
    ///
    /// The same collapse the render loop's hot path makes ([`sample_scene`]),
    /// exposed for a host that draws its own frames instead of listening for
    /// the loop's broadcast. [`Scene::render`] is pure in `t` and seek-safe, so
    /// an out-of-order or repeated sample is as valid as an in-order one; the
    /// only shared state is the scratch arena, which is why this takes the same
    /// lock the loop does rather than handing the scene out.
    pub fn sample(&self, t: f32) -> Option<UniverseState> {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        let base = if !guard.perform_deck_states.is_empty() {
            Some(render_perform_mix(&mut guard))
        } else {
            let inner = &mut *guard;
            inner
                .active_scene
                .as_ref()
                .map(|scene| sample_scene(scene, t, &mut inner.scratch))
        };
        let inner = &mut *guard;
        let Some(live) = inner.live.as_mut() else {
            return base;
        };
        let now = Instant::now();
        let time = match live.clock() {
            LiveClock::Deck(id) => inner
                .perform_deck_states
                .iter()
                .find(|state| state.deck_id == id)
                .map(|state| state.clock.position_at(now) as f32 + inner.perform_latency),
            LiveClock::Tap => Some(live.tap_time(now)),
        };
        let Some(time) = time else {
            // Its deck is gone; until live follow recompiles the set, it waits.
            return base;
        };
        // The bank reads the set's clock whether or not anything plays: color
        // cells step on it and presses land on it.
        live.tick(time, now);
        if !live.update_levels() {
            return base;
        }
        let base = base.unwrap_or_default();
        Some(live.render_onto(base, time, now, &mut inner.scratch))
    }

    /// Play `layers` in the Perform set, the set with patterns compiled
    /// again since, if the set is still build `id`.
    pub(crate) fn replace_live(&self, id: u64, layers: Arc<crate::perform::live::Layers>) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        if let Some(live) = guard.live.as_mut().filter(|live| live.id() == id) {
            live.replace(layers);
        }
    }

    /// Updates belong to one engine and one render slot. Starting a newer
    /// update invalidates the previous one before its asynchronous reads begin.
    pub(crate) fn begin_scene_update(&self, target: SceneTarget) -> SceneUpdate {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.scene_generation += 1;
        let generation = guard.scene_generation;
        guard.pending_scenes.insert(target, generation);
        SceneUpdate { target, generation }
    }

    /// The scene on show in the active slot. Clones share what it has learnt
    /// of when its heads are lit.
    pub(crate) fn active_scene(&self) -> Option<Scene> {
        self.inner
            .lock()
            .expect("render engine poisoned")
            .active_scene
            .clone()
    }

    /// Begin showing `score_id` in the active slot: a [`Self::begin_scene_update`]
    /// that also records whose scene the slot now holds.
    pub(crate) fn begin_score_update(&self, score_id: &str) -> SceneUpdate {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.active_score = Some(score_id.to_owned());
        guard.scene_generation += 1;
        let generation = guard.scene_generation;
        guard.pending_scenes.insert(SceneTarget::Active, generation);
        SceneUpdate {
            target: SceneTarget::Active,
            generation,
        }
    }

    /// Stop showing `score_id`: drop its scene and any build of it still
    /// compiling. Does nothing when another score has taken the slot since, so
    /// a close that arrives after the next score opened cannot blank it.
    pub(crate) fn leave_score(&self, score_id: &str) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        if guard.active_score.as_deref() != Some(score_id) {
            return;
        }
        guard.active_score = None;
        guard.active_scene = None;
        guard.pending_scenes.remove(&SceneTarget::Active);
    }

    /// Check and install under the same lock, unless a newer update for the
    /// same slot began since `update` did: a late result cannot resurrect a
    /// closed score/deck or clear the editor while compiling performance output.
    pub(crate) fn finish_update(&self, update: SceneUpdate, built: Built) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        if guard.pending_scenes.get(&update.target) != Some(&update.generation) {
            return;
        }
        guard.pending_scenes.remove(&update.target);
        match (update.target, built) {
            (SceneTarget::Active, Built::Scene(scene)) => {
                guard.active_scene = (!scene.is_empty()).then_some(scene);
            }
            (SceneTarget::Deck(id), Built::Scene(scene)) => {
                if scene.is_empty() {
                    guard.perform_layers.remove(&id);
                } else {
                    guard.perform_layers.insert(id, scene);
                }
            }
            (SceneTarget::Live, Built::Live(set)) => guard.live = set,
            (target, _) => debug_assert!(false, "{target:?} finished with another slot's output"),
        }
    }

    pub fn set_perform_deck_states(&self, states: Vec<PerformDeckInput>) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.perform_deck_states = states;
    }

    /// Drop a deck's layer and any build still in flight for it: the deck
    /// loaded another track.
    pub(crate) fn clear_deck(&self, id: u8) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.pending_scenes.remove(&SceneTarget::Deck(id));
        guard.perform_layers.remove(&id);
    }

    pub fn set_perform_latency(&self, seconds: f32) {
        self.inner
            .lock()
            .expect("render engine poisoned")
            .perform_latency = seconds;
    }

    pub fn clear_perform(&self) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        log::debug!(
            "[render] clear_perform: clearing {} deck layers and {} deck states",
            guard.perform_layers.len(),
            guard.perform_deck_states.len()
        );
        guard.perform_layers.clear();
        guard
            .pending_scenes
            .retain(|target, _| matches!(target, SceneTarget::Active | SceneTarget::Live));
        guard.perform_deck_states.clear();
    }
}

// ============================================================================
// Perform mix
// ============================================================================

/// Render each deck's layer at its current time and blend by volume.
fn render_perform_mix(guard: &mut RenderEngineInner) -> UniverseState {
    score_mix(
        &guard.perform_layers,
        &guard.perform_deck_states,
        Instant::now(),
        guard.perform_latency,
        &mut guard.scratch,
    )
}

/// Score-only blend: evaluate each deck's scene at its time, weighted-average by
/// deck volume. The per-deck composite is the unified `Scene::render`.
fn score_mix(
    layers: &HashMap<u8, Scene>,
    deck_states: &[PerformDeckInput],
    now: Instant,
    latency: f32,
    scratch: &mut Arena,
) -> UniverseState {
    let mut frames: Vec<(UniverseState, f32)> = Vec::new();
    for ds in deck_states {
        if ds.volume <= 0.0 {
            continue;
        }
        if let Some(scene) = layers.get(&ds.deck_id) {
            let time = ds.clock.position_at(now) as f32 + latency;
            frames.push((sample_scene(scene, time, scratch), ds.volume));
        }
    }

    if frames.is_empty() {
        return UniverseState::default();
    }

    if frames.len() == 1 {
        return frames.into_iter().next().unwrap().0;
    }

    let total_volume: f32 = frames.iter().map(|(_, v)| *v).sum();
    if total_volume <= 0.0 {
        return UniverseState::default();
    }

    let mut all_keys = std::collections::HashSet::new();
    for (state, _) in &frames {
        all_keys.extend(state.primitives.keys().cloned());
    }

    let mut blended = HashMap::with_capacity(all_keys.len());
    for key in all_keys {
        let mut dimmer = 0.0f32;
        let mut color = [0.0f32; 3];
        let mut strobe = 0.0f32;
        let mut speed = 0.0f32;

        let mut best_position = [0.0f32; 2];
        let mut best_aim = None;
        let mut best_vol = -1.0f32;

        for (state, vol) in &frames {
            let w = vol / total_volume;
            if let Some(prim) = state.primitives.get(&key) {
                dimmer += prim.dimmer * w;
                color[0] += prim.color[0] * w;
                color[1] += prim.color[1] * w;
                color[2] += prim.color[2] * w;
                strobe += prim.strobe * w;
                speed += prim.speed * w;

                if *vol > best_vol {
                    best_vol = *vol;
                    best_position = prim.position;
                    best_aim = prim.aim;
                }
            }
        }

        blended.insert(
            key,
            PrimitiveState {
                dimmer: dimmer.clamp(0.0, 1.0),
                color,
                strobe: strobe.clamp(0.0, 1.0),
                position: best_position,
                speed: if speed > 0.5 { 1.0 } else { 0.0 },
                aim: best_aim,
            },
        );
    }

    // Each deck's laser elements show at its share of the volume, on
    // layers of their own.
    let lasers = frames
        .iter()
        .enumerate()
        .flat_map(|(deck, (state, vol))| {
            let w = f64::from(vol / total_volume);
            state
                .lasers
                .iter()
                .map(move |element| luma_patterns::laser::Element {
                    layer: element.layer | ((deck as u32) << 24),
                    intensity: element.intensity * w,
                    ..element.clone()
                })
        })
        .collect();
    UniverseState {
        primitives: blended,
        lasers,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn scene_updates_cannot_overwrite_newer_scores_or_reopen_closed_slots() {
        use super::{Built, RenderEngine, SceneTarget};
        use crate::eval::Scene;
        let engine = RenderEngine::default();
        engine.inner.lock().unwrap().active_scene = Some(Scene::default());
        let stale = engine.begin_scene_update(SceneTarget::Active);
        let current = engine.begin_scene_update(SceneTarget::Active);
        engine.finish_update(stale, Built::Scene(Scene::default()));
        assert!(
            engine.sample(0.0).is_some(),
            "a superseded build installs nothing"
        );
        engine.finish_update(current, Built::Scene(Scene::default()));
        assert!(
            engine.sample(0.0).is_none(),
            "an empty build clears the slot"
        );

        // Leaving the editor is an update of its own, so it supersedes a build
        // still compiling.
        let pending = engine.begin_scene_update(SceneTarget::Active);
        let leave = engine.begin_scene_update(SceneTarget::Active);
        engine.finish_update(leave, Built::Scene(Scene::default()));
        engine.inner.lock().unwrap().active_scene = Some(Scene::default());
        engine.finish_update(pending, Built::Scene(Scene::default()));
        assert!(engine.sample(0.0).is_some());
        assert!(engine.inner.lock().unwrap().pending_scenes.is_empty());
    }

    #[test]
    fn leaving_a_score_clears_its_scene_and_no_other() {
        use super::{Built, RenderEngine};
        use crate::eval::Scene;
        let installed = |engine: &RenderEngine| {
            engine.inner.lock().unwrap().active_scene = Some(Scene::default());
        };
        // An empty build clears the slot when it installs, so "still lit"
        // after one means the build was turned away.
        let engine = RenderEngine::default();

        // The score on show closes: its scene goes, and a build of it that
        // was still compiling cannot bring it back.
        let compiling = engine.begin_score_update("a");
        installed(&engine);
        engine.leave_score("a");
        assert!(engine.sample(0.0).is_none(), "the closed score went dark");
        installed(&engine);
        engine.finish_update(compiling, Built::Scene(Scene::default()));
        assert!(
            engine.sample(0.0).is_some(),
            "its late build was turned away"
        );

        // A close that arrives after the next score opened touches nothing:
        // neither that score's scene nor the build of it in flight.
        let next = engine.begin_score_update("b");
        engine.leave_score("a");
        assert!(engine.sample(0.0).is_some(), "the next score stayed lit");
        engine.finish_update(next, Built::Scene(Scene::default()));
        assert!(engine.sample(0.0).is_none(), "and its build still landed");
    }

    #[test]
    fn deck_scene_updates_are_independent_and_empty_results_clear_old_output() {
        use super::{Built, RenderEngine, SceneTarget};
        use crate::eval::Scene;
        let engine = RenderEngine::default();
        engine.inner.lock().unwrap().active_scene = Some(Scene::default());
        engine
            .inner
            .lock()
            .unwrap()
            .perform_layers
            .insert(1, Scene::default());
        let old = engine.begin_scene_update(SceneTarget::Deck(1));
        let current = engine.begin_scene_update(SceneTarget::Deck(1));
        let other = engine.begin_scene_update(SceneTarget::Deck(2));
        engine.finish_update(old, Built::Scene(Scene::default()));
        assert!(engine.inner.lock().unwrap().perform_layers.contains_key(&1));
        engine.finish_update(current, Built::Scene(Scene::default()));
        assert!(!engine.inner.lock().unwrap().perform_layers.contains_key(&1));
        assert!(
            engine.sample(0.0).is_some(),
            "deck changes preserve the editor"
        );
        engine.clear_perform();
        engine.finish_update(other, Built::Scene(Scene::default()));
        assert!(engine.inner.lock().unwrap().pending_scenes.is_empty());
        assert!(engine.sample(0.0).is_some());
    }
}
