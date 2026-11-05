//! Render Engine
//!
//! Owns all rendering state (layers, universe generation, DMX output).
//! Decoupled from audio playback — reads time from HostAudioState only in
//! edit mode. In perform mode it renders per-deck layers and blends by volume.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};
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

/// Blink-twice identify sequence for one or more targets. A target is a
/// member key: `"{fixture_id}"` (whole fixture) or `"{fixture_id}:{head}"`
/// (single head).
struct IdentifyState {
    targets: Vec<String>,
    start: Instant,
}

/// Two blinks over 0.6s: ON 0–0.15, OFF 0.15–0.3, ON 0.3–0.45, OFF 0.45–0.6
const IDENTIFY_DURATION: f32 = 0.6;

fn identify_dimmer(elapsed: f32) -> f32 {
    if (elapsed < 0.15) || (elapsed >= 0.3 && elapsed < 0.45) {
        1.0
    } else {
        0.0
    }
}

pub(crate) struct RenderEngineInner {
    scene_generation: u64,
    pending_scenes: HashMap<SceneTarget, u64>,
    /// Active scene for track editor / pattern editor (composited per frame).
    active_scene: Option<Scene>,
    /// Per-deck scenes for perform mode (the track's full composite per deck).
    perform_layers: HashMap<u8, Scene>,
    /// Per-deck clocks and blend weights, set by live follow.
    perform_deck_states: Vec<PerformDeckInput>,
    /// Seconds added to every deck's position: positive lights lead the music.
    perform_latency: f32,
    /// The Perform set, over the decks or the editor.
    live: Option<LiveSet>,
    /// Fixture identify blink (highest priority)
    identify: Option<IdentifyState>,
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
                perform_layers: HashMap::new(),
                perform_deck_states: Vec::new(),
                perform_latency: 0.0,
                live: None,
                identify: None,
                scratch: Arena::default(),
            })),
        }
    }
}

impl RenderEngine {
    pub fn reset_for_identity_switch(&self) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.active_scene = None;
        guard.pending_scenes.clear();
        guard.perform_layers.clear();
        guard.perform_deck_states.clear();
        guard.live = None;
        guard.identify = None;
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
        if let Some(identify) = &guard.identify {
            let elapsed = identify.start.elapsed().as_secs_f32();
            if elapsed < IDENTIFY_DURATION {
                let blink = PrimitiveState {
                    dimmer: identify_dimmer(elapsed),
                    color: [1.0, 1.0, 1.0],
                    strobe: 0.0,
                    position: [0.0, 0.0],
                    speed: 0.0,
                    aim: None,
                };
                let mut primitives = HashMap::new();
                for target in &identify.targets {
                    if target.contains(':') {
                        primitives.insert(target.clone(), blink.clone());
                    } else {
                        for head in 0..16 {
                            primitives.insert(format!("{target}:{head}"), blink.clone());
                        }
                    }
                }
                return Some(UniverseState {
                    primitives,
                    lasers: Vec::new(),
                });
            }
            guard.identify = None;
        }
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
        if !live.update_levels() {
            return base;
        }
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
        let base = base.unwrap_or_else(|| UniverseState {
            primitives: HashMap::new(),
            lasers: Vec::new(),
        });
        Some(live.render_onto(base, time, &mut inner.scratch))
    }

    /// Install a compiled Perform set, unless a newer build began since
    /// `update` did. `None` clears it.
    pub(crate) fn finish_live_update(&self, update: SceneUpdate, set: Option<LiveSet>) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        if guard.pending_scenes.get(&update.target) != Some(&update.generation) {
            return;
        }
        guard.pending_scenes.remove(&update.target);
        guard.live = set;
    }

    pub fn set_active_scene(&self, scene: Option<Scene>) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.pending_scenes.remove(&SceneTarget::Active);
        guard.active_scene = scene;
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

    /// Check and install under the same lock; a late result cannot resurrect a
    /// closed score/deck or clear the editor while compiling performance output.
    pub(crate) fn finish_scene_update(&self, update: SceneUpdate, scene: Scene) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        if guard.pending_scenes.get(&update.target) != Some(&update.generation) {
            return;
        }
        guard.pending_scenes.remove(&update.target);
        match update.target {
            SceneTarget::Active => guard.active_scene = (!scene.is_empty()).then_some(scene),
            SceneTarget::Deck(id) => {
                if scene.is_empty() {
                    guard.perform_layers.remove(&id);
                } else {
                    guard.perform_layers.insert(id, scene);
                }
            }
            // A live layer installs with `finish_live_update`.
            SceneTarget::Live => {}
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

    /// Targets are member keys: `"fid"` (whole fixture) or `"fid:N"` (one head).
    pub fn identify_targets(&self, targets: Vec<String>) {
        if targets.is_empty() {
            return;
        }
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.identify = Some(IdentifyState {
            targets,
            start: Instant::now(),
        });
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
        return UniverseState {
            primitives: HashMap::new(),
            lasers: Vec::new(),
        };
    }

    if frames.len() == 1 {
        return frames.into_iter().next().unwrap().0;
    }

    let total_volume: f32 = frames.iter().map(|(_, v)| *v).sum();
    if total_volume <= 0.0 {
        return UniverseState {
            primitives: HashMap::new(),
            lasers: Vec::new(),
        };
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

/// Resolve caller-supplied primitive targets through their fixture rows, then
/// retain one admitted venue snapshot until the identify effect is installed.
/// A mixed-venue or unknown target fails as one opaque not-found result.
pub(crate) async fn authorize_identify_targets<'a>(
    pool: &'a sqlx::SqlitePool,
    targets: &[String],
) -> Result<VenueAccess<'a, Read>, String> {
    let fixture_ids = targets
        .iter()
        .map(|target| identify_fixture_id(target))
        .collect::<Result<Vec<_>, _>>()?;
    let first = fixture_ids
        .first()
        .ok_or_else(|| "Identify requires at least one fixture target".to_string())?;
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Fixture(first)).await?;
    let venue_id = access.venue_id().to_owned();

    for fixture_id in fixture_ids.into_iter().skip(1) {
        let belongs: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM fixtures WHERE id = ? AND venue_id = ?")
                .bind(fixture_id)
                .bind(&venue_id)
                .fetch_optional(access.connection())
                .await
                .map_err(|error| format!("Failed to authorize identify target: {error}"))?;
        if belongs.is_none() {
            return Err("Venue resource not found".into());
        }
    }
    Ok(access)
}

fn identify_fixture_id(target: &str) -> Result<&str, String> {
    let (fixture_id, head) = match target.split_once(':') {
        Some((fixture_id, head)) => (fixture_id, Some(head)),
        None => (target, None),
    };
    if fixture_id.is_empty()
        || head.is_some_and(|head| head.is_empty() || head.parse::<usize>().is_err())
    {
        return Err("Invalid identify target".into());
    }
    Ok(fixture_id)
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    use super::{authorize_identify_targets, identify_fixture_id};

    #[test]
    fn native_sampling_identifies_then_returns_to_the_scene() {
        let engine = super::RenderEngine::default();
        engine.set_active_scene(Some(crate::eval::Scene::default()));
        engine.identify_targets(vec!["fixture:3".into(), "multi".into()]);
        let frame = engine.sample(0.0).unwrap();
        assert_eq!(frame.primitives.len(), 17);
        assert!(frame.primitives.contains_key("fixture:3"));
        assert!(frame.primitives.contains_key("multi:15"));
        engine
            .inner
            .lock()
            .unwrap()
            .identify
            .as_mut()
            .unwrap()
            .start = std::time::Instant::now() - std::time::Duration::from_secs(10);
        assert!(engine.sample(0.0).unwrap().primitives.is_empty());
        assert!(engine.inner.lock().unwrap().identify.is_none());
    }

    #[test]
    fn scene_updates_cannot_overwrite_newer_scores_or_reopen_closed_slots() {
        use super::{RenderEngine, SceneTarget};
        use crate::eval::Scene;
        let engine = RenderEngine::default();
        let stale = engine.begin_scene_update(SceneTarget::Active);
        let current = engine.begin_scene_update(SceneTarget::Active);
        engine.set_active_scene(Some(Scene::default()));
        engine.finish_scene_update(stale, Scene::default());
        engine.finish_scene_update(current, Scene::default());
        assert!(
            engine.sample(0.0).is_some(),
            "direct scene replacement invalidates pending compilation"
        );

        let pending = engine.begin_scene_update(SceneTarget::Active);
        engine.set_active_scene(None);
        engine.finish_scene_update(pending, Scene::default());
        assert!(engine.sample(0.0).is_none());
        assert!(engine.inner.lock().unwrap().pending_scenes.is_empty());
    }

    #[test]
    fn deck_scene_updates_are_independent_and_empty_results_clear_old_output() {
        use super::{RenderEngine, SceneTarget};
        use crate::eval::Scene;
        let engine = RenderEngine::default();
        engine.set_active_scene(Some(Scene::default()));
        engine
            .inner
            .lock()
            .unwrap()
            .perform_layers
            .insert(1, Scene::default());
        let old = engine.begin_scene_update(SceneTarget::Deck(1));
        let current = engine.begin_scene_update(SceneTarget::Deck(1));
        let other = engine.begin_scene_update(SceneTarget::Deck(2));
        engine.finish_scene_update(old, Scene::default());
        assert!(engine.inner.lock().unwrap().perform_layers.contains_key(&1));
        engine.finish_scene_update(current, Scene::default());
        assert!(!engine.inner.lock().unwrap().perform_layers.contains_key(&1));
        assert!(
            engine.sample(0.0).is_some(),
            "deck changes preserve the editor"
        );
        engine.clear_perform();
        engine.finish_scene_update(other, Scene::default());
        assert!(engine.inner.lock().unwrap().pending_scenes.is_empty());
        assert!(engine.sample(0.0).is_some());
    }

    async fn identify_test_pool() -> (tempfile::TempDir, sqlx::SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("render-identify.db"))
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO venues (id, uid, name) VALUES
                ('venue-a', 'alice', 'Venue A'),
                ('venue-b', 'alice', 'Venue B');
             INSERT INTO fixtures
                (id, uid, venue_id, address, num_channels, manufacturer, model, mode_name, fixture_path)
             VALUES
                ('fixture-a', 'alice', 'venue-a', 1, 1, 'Test', 'A', 'Default', 'a.json'),
                ('fixture-a2', 'alice', 'venue-a', 2, 1, 'Test', 'A2', 'Default', 'a2.json'),
                ('fixture-b', 'alice', 'venue-b', 3, 1, 'Test', 'B', 'Default', 'b.json')",
        )
        .execute(&pool)
        .await
        .unwrap();
        (directory, pool)
    }

    #[test]
    fn identify_target_parser_accepts_only_fixture_and_numeric_head_forms() {
        assert_eq!(identify_fixture_id("fixture-a").unwrap(), "fixture-a");
        assert_eq!(identify_fixture_id("fixture-a:0").unwrap(), "fixture-a");
        assert_eq!(identify_fixture_id("fixture-a:42").unwrap(), "fixture-a");

        for invalid in ["", ":0", "fixture-a:", "fixture-a:head", "fixture-a:0:1"] {
            assert!(
                identify_fixture_id(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[tokio::test]
    async fn identify_authorization_requires_one_admitted_venue_for_every_target() {
        let (_directory, pool) = identify_test_pool().await;

        let access =
            authorize_identify_targets(&pool, &["fixture-a:0".into(), "fixture-a2".into()])
                .await
                .unwrap();
        assert_eq!(access.venue_id(), "venue-a");
        drop(access);

        let mixed = authorize_identify_targets(&pool, &["fixture-a".into(), "fixture-b".into()])
            .await
            .err()
            .unwrap();
        assert_eq!(mixed, "Venue resource not found");

        let unknown = authorize_identify_targets(&pool, &["fixture-a".into(), "missing".into()])
            .await
            .err()
            .unwrap();
        assert_eq!(unknown, "Venue resource not found");

        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();
        let unauthorized = authorize_identify_targets(&pool, &["fixture-a".into()])
            .await
            .err()
            .unwrap();
        assert_eq!(unauthorized, "Venue resource not found");
    }
}
