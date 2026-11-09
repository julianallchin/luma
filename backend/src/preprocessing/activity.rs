//! What analysis is doing right now: which tracks a batch still holds, which
//! steps run on them, and which steps a batch has paused.
//!
//! In memory, and only a report: the scheduler registers its work here as it
//! goes, through guards that let go on every path out of it, cancellation
//! included, and the status bar reads [`AnalysisActivity::snapshot`]. Nothing
//! here decides what runs.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// One host's analysis activity, shared by every batch it admits.
#[derive(Clone, Debug, Default)]
pub struct AnalysisActivity(Arc<Inner>);

#[derive(Debug, Default)]
struct Inner {
    state: Mutex<State>,
    batches: AtomicU64,
}

#[derive(Debug, Default)]
struct State {
    tracks: HashMap<String, TrackState>,
    /// Steps a batch has paused, by batch, with why.
    paused: HashMap<u64, BTreeMap<&'static str, String>>,
}

#[derive(Debug, Default)]
struct TrackState {
    /// Batches that still hold the track: queued or running in each.
    batches: usize,
    /// Steps running on it now. A step may run twice at once only across
    /// batches, so this is a multiset.
    running: Vec<&'static str>,
}

/// One track's analysis, as [`AnalysisActivity::snapshot`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackActivity {
    pub track_id: String,
    /// The steps running on it now; empty while it waits its turn.
    pub running: Vec<&'static str>,
}

/// What analysis is doing, at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivitySnapshot {
    /// Tracks with work in flight or queued, by track id.
    pub tracks: Vec<TrackActivity>,
    /// Steps a running batch skips on the rest of its tracks, with why.
    pub paused: Vec<(&'static str, String)>,
}

impl AnalysisActivity {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hold `track_ids` as queued for one batch, until the batch says each is
    /// done or the returned guard drops.
    #[must_use]
    pub fn batch(&self, track_ids: &[String]) -> Batch {
        let id = self.0.batches.fetch_add(1, Ordering::Relaxed);
        let mut held = HashMap::new();
        let mut state = self.state();
        for track_id in track_ids {
            *held.entry(track_id.clone()).or_insert(0) += 1;
            state.tracks.entry(track_id.clone()).or_default().batches += 1;
        }
        drop(state);
        Batch {
            activity: self.clone(),
            id,
            held: Mutex::new(held),
        }
    }

    /// Mark `step` running on `track_id` until the returned guard drops.
    #[must_use]
    pub fn running(&self, track_id: &str, step: &'static str) -> Running {
        self.state()
            .tracks
            .entry(track_id.to_owned())
            .or_default()
            .running
            .push(step);
        Running {
            activity: self.clone(),
            track_id: track_id.to_owned(),
            step,
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> ActivitySnapshot {
        let state = self.state();
        let mut tracks: Vec<TrackActivity> = state
            .tracks
            .iter()
            .map(|(track_id, track)| {
                let mut running = track.running.clone();
                running.sort_unstable();
                running.dedup();
                TrackActivity {
                    track_id: track_id.clone(),
                    running,
                }
            })
            .collect();
        tracks.sort_by(|a, b| a.track_id.cmp(&b.track_id));
        let mut paused: BTreeMap<&'static str, String> = BTreeMap::new();
        for steps in state.paused.values() {
            for (step, reason) in steps {
                paused.entry(step).or_insert_with(|| reason.clone());
            }
        }
        ActivitySnapshot {
            tracks,
            paused: paused.into_iter().collect(),
        }
    }

    /// Forget `track_id` once nothing holds it.
    fn release(state: &mut State, track_id: &str) {
        if let Some(track) = state.tracks.get(track_id) {
            if track.batches == 0 && track.running.is_empty() {
                state.tracks.remove(track_id);
            }
        }
    }
}

/// One batch's hold on its tracks and its paused steps. Dropping it lets go
/// of whatever it still holds.
#[derive(Debug)]
pub struct Batch {
    activity: AnalysisActivity,
    id: u64,
    /// The tracks this batch still holds, each with how many times it was
    /// listed.
    held: Mutex<HashMap<String, usize>>,
}

impl Batch {
    /// This batch has finished with `track_id`.
    pub fn finish(&self, track_id: &str) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(count) = held.get_mut(track_id) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            held.remove(track_id);
        }
        drop(held);
        let mut state = self.activity.state();
        if let Some(track) = state.tracks.get_mut(track_id) {
            track.batches = track.batches.saturating_sub(1);
        }
        AnalysisActivity::release(&mut state, track_id);
    }

    /// The rest of this batch skips `step`, for `reason`.
    pub fn pause(&self, step: &'static str, reason: String) {
        self.activity
            .state()
            .paused
            .entry(self.id)
            .or_default()
            .insert(step, reason);
    }

    /// `step` worked again in this batch, so it is no longer paused.
    pub fn resume(&self, step: &str) {
        let mut state = self.activity.state();
        if let Some(steps) = state.paused.get_mut(&self.id) {
            steps.remove(step);
            if steps.is_empty() {
                state.paused.remove(&self.id);
            }
        }
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        let held = std::mem::take(self.held.get_mut().unwrap_or_else(PoisonError::into_inner));
        let mut state = self.activity.state();
        state.paused.remove(&self.id);
        for (track_id, count) in held {
            if let Some(track) = state.tracks.get_mut(&track_id) {
                track.batches = track.batches.saturating_sub(count);
            }
            AnalysisActivity::release(&mut state, &track_id);
        }
    }
}

/// A step running on a track, until this drops.
#[derive(Debug)]
pub struct Running {
    activity: AnalysisActivity,
    track_id: String,
    step: &'static str,
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut state = self.activity.state();
        if let Some(track) = state.tracks.get_mut(&self.track_id) {
            if let Some(at) = track.running.iter().position(|step| *step == self.step) {
                track.running.swap_remove(at);
            }
        }
        AnalysisActivity::release(&mut state, &self.track_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    fn tracks(activity: &AnalysisActivity) -> Vec<(String, Vec<&'static str>)> {
        activity
            .snapshot()
            .tracks
            .into_iter()
            .map(|track| (track.track_id, track.running))
            .collect()
    }

    /// A batch's tracks read as queued, then running, then gone once the
    /// batch is done with them.
    #[test]
    fn a_batch_holds_its_tracks_until_each_is_done() {
        let activity = AnalysisActivity::default();
        let batch = activity.batch(&ids(&["a", "b"]));
        assert_eq!(
            tracks(&activity),
            vec![("a".into(), vec![]), ("b".into(), vec![])]
        );

        let running = activity.running("a", "stems");
        assert_eq!(tracks(&activity)[0], ("a".into(), vec!["stems"]));
        drop(running);
        batch.finish("a");
        assert_eq!(tracks(&activity), vec![("b".into(), vec![])]);

        drop(batch);
        assert_eq!(activity.snapshot(), ActivitySnapshot::default());
    }

    /// Two batches holding one track: it stays until both let go.
    #[test]
    fn a_track_in_two_batches_stays_until_both_let_go() {
        let activity = AnalysisActivity::default();
        let import = activity.batch(&ids(&["a"]));
        let reconcile = activity.batch(&ids(&["a"]));
        import.finish("a");
        assert_eq!(tracks(&activity).len(), 1);
        drop(reconcile);
        assert!(tracks(&activity).is_empty());
        // Finishing again, or dropping, after the track is gone does nothing.
        import.finish("a");
        drop(import);
        assert!(tracks(&activity).is_empty());
    }

    /// A paused step reads as paused while its batch runs, and not once the
    /// step works again or the batch ends.
    #[test]
    fn a_pause_lasts_as_long_as_its_batch() {
        let activity = AnalysisActivity::default();
        let batch = activity.batch(&ids(&["a"]));
        batch.pause("stems", "out of memory".into());
        assert_eq!(
            activity.snapshot().paused,
            vec![("stems", "out of memory".to_owned())]
        );
        batch.resume("stems");
        assert!(activity.snapshot().paused.is_empty());

        batch.pause("stems", "out of memory".into());
        drop(batch);
        assert!(activity.snapshot().paused.is_empty());
    }
}
