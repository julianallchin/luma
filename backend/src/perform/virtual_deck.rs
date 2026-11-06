//! The virtual deck: the app's own audio player as a deck.
//!
//! For practising and seeing a venue's patterns without DJ gear. Whatever
//! library track the host player has loaded becomes deck 1, pinned to that
//! track rather than matched by name, and the player's playhead is its
//! position. The deck then runs through the same follower, scene and render
//! path as a deck on a CDJ.
//!
//! The player is the one the track editor and the add-tracks audition use, so
//! whichever loaded last is what the deck follows. A file being auditioned is
//! not a library track and gives no deck.

use std::sync::Arc;
use std::time::Instant;

use tokio::task::JoinHandle;

use super::{LoadedTrack, Reading, Shared, TICK};
use crate::host_audio::HostAudioState;

/// The one deck a virtual source has.
pub(crate) const DECK: u8 = 1;

/// Follow the host player as [`DECK`] for `run`, until the task is aborted.
pub(crate) fn follow(shared: Arc<Shared>, host: HostAudioState, run: u64) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(TICK);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut loaded: Option<(String, LoadedTrack)> = None;
        loop {
            ticks.tick().await;
            let snapshot = host.snapshot();
            let track_id = snapshot
                .track_id
                .filter(|id| snapshot.is_loaded && !id.starts_with("file:"));
            if track_id.as_deref() != loaded.as_ref().map(|(id, _)| id.as_str()) {
                loaded = match track_id {
                    Some(id) => Some((id.clone(), describe(&shared, &id).await)),
                    None => None,
                };
                match &loaded {
                    Some((id, track)) => {
                        shared.ingest(
                            run,
                            Some(reading(track, &host, snapshot.is_playing)),
                            Instant::now(),
                        );
                        // Pinned, not matched: the player says which track
                        // this is, so a name lookup could only get it wrong.
                        let _ = Shared::pin(&shared, run, DECK, Some(id.clone()));
                    }
                    None => shared.remove_deck(run, DECK),
                }
                continue;
            }
            if let Some((_, track)) = &loaded {
                shared.ingest(
                    run,
                    Some(reading(track, &host, snapshot.is_playing)),
                    Instant::now(),
                );
            }
        }
    })
}

/// The track as a source would describe it, for the deck card.
async fn describe(shared: &Shared, track_id: &str) -> LoadedTrack {
    let summary = crate::database::local::tracks::get_track_by_id(&shared.pool, track_id)
        .await
        .ok()
        .flatten();
    LoadedTrack {
        title: summary
            .as_ref()
            .and_then(|track| track.title.clone())
            .unwrap_or_default(),
        artist: summary.and_then(|track| track.artist).unwrap_or_default(),
        network_path: None,
        duration: None,
        bpm: None,
    }
}

fn reading(track: &LoadedTrack, host: &HostAudioState, playing: bool) -> Reading {
    Reading {
        deck: DECK,
        track: Some(track.clone()),
        // Absolute track time: the editor may have loaded only a segment.
        position: Some(host.render_time() as f64),
        playing: Some(playing),
        bpm: 0.0,
        rate: 1.0,
        volume: 1.0,
    }
}
