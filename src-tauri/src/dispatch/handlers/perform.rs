//! Live DJ decks: hardware telemetry (StageLinQ / Pro DJ Link), matching a
//! deck's loaded track against the library, and compositing a matched or
//! unmatched deck onto a perform slot.

use std::collections::HashSet;

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::models::perform::PerformTrackMatch;

// ── StageLinQ (Denon / Engine DJ) ─────────────────────────────────────────────

/// Start the StageLinQ listener. Deck telemetry arrives on `perform_event`, so
/// a caller that has not subscribed before this returns loses the first
/// `DeviceDiscovered` / `Connected`.
pub async fn stagelinq_connect(services: &AppServices) -> Result<(), CommandError> {
    Ok(services.stagelinq.start(services.events.clone()).await?)
}

/// Stop the StageLinQ listener. Idempotent — stopping when not running is fine.
pub async fn stagelinq_disconnect(services: &AppServices) -> Result<(), CommandError> {
    Ok(services.stagelinq.stop().await?)
}

// ── Pro DJ Link (Pioneer) ─────────────────────────────────────────────────────

/// Passively listen for CDJ keepalives for 3 seconds and return what was heard.
/// Safe to call while not connected — it performs no device-number claim.
///
/// An empty list means "nothing heard"; a failure to bind the socket also
/// yields an empty list, so the two are indistinguishable to the caller.
pub async fn prodjlink_discover(
    _services: &AppServices,
) -> Result<Vec<prodjlink::DiscoveredDevice>, CommandError> {
    Ok(prodjlink::discover_cdjs(3000).await)
}

/// Start the Pro DJ Link listener, claiming `device_num` as this app's virtual
/// player number. It must not collide with a real deck on the network.
pub async fn prodjlink_connect(services: &AppServices, device_num: u8) -> Result<(), CommandError> {
    Ok(services
        .prodjlink
        .start(services.events.clone(), device_num)
        .await?)
}

/// Stop the Pro DJ Link listener. Idempotent, like its StageLinQ mirror.
pub async fn prodjlink_disconnect(services: &AppServices) -> Result<(), CommandError> {
    Ok(services.prodjlink.stop().await?)
}

// ── Track matching ────────────────────────────────────────────────────────────

/// Match a StageLinQ deck's loaded track by the filename in its network path.
///
/// The track lookup is library-wide but the annotation lookup is venue-scoped,
/// so a track can match with `has_annotations` false purely because its score
/// lives in another venue.
pub async fn perform_match_track(
    services: &AppServices,
    track_network_path: String,
    venue_id: String,
) -> Result<PerformTrackMatch, CommandError> {
    let pool = &services.db.0;
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;

    let Some(filename) = stagelinq::extract_filename_from_network_path(&track_network_path) else {
        return Ok(PerformTrackMatch::miss(String::new()));
    };
    let filename = filename.to_string();

    let tracks = crate::database::local::tracks::get_tracks_by_source_filename(pool, &filename)
        .await
        .map_err(CommandError::Internal)?;

    // No tie-break: duplicate filenames resolve to an arbitrary row.
    let Some(track) = tracks.first() else {
        return Ok(PerformTrackMatch::miss(filename));
    };

    let scores = crate::database::local::scores::get_scores_for_track(&mut access, &track.id)
        .await
        .map_err(CommandError::Internal)?;

    Ok(PerformTrackMatch {
        track_id: Some(track.id.clone()),
        has_annotations: !scores.is_empty(),
        filename,
    })
}

/// Match a Pro DJ Link deck's loaded track by metadata, since CDJs report no
/// usable filename.
///
/// The cascade is ordered and each stage narrows the last:
///   1. duration within ±5 s,
///   2. BPM within 5%, allowing the ×2 / ÷2 harmonic that analyzers disagree on,
///   3. bigram similarity of the normalized `title artist` string, best wins
///      above 0.25.
pub async fn perform_match_track_by_metadata(
    services: &AppServices,
    title: String,
    artist: String,
    bpm: f64,
    duration_secs: f64,
    venue_id: String,
) -> Result<PerformTrackMatch, CommandError> {
    let pool = &services.db.0;
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
    if title.is_empty() && artist.is_empty() {
        return Ok(PerformTrackMatch::miss(String::new()));
    }

    let candidates =
        crate::database::local::tracks::get_tracks_by_duration(pool, duration_secs, 5.0)
            .await
            .map_err(CommandError::Internal)?;

    let bpm_filtered: Vec<_> = candidates
        .into_iter()
        .filter(|t| bpm_matches(t.bpm.unwrap_or(0.0), bpm))
        .collect();

    if bpm_filtered.is_empty() {
        return Ok(PerformTrackMatch::miss(String::new()));
    }

    let query = normalize_for_match(&format!("{title} {artist}"));
    let mut scored: Vec<_> = bpm_filtered
        .iter()
        .map(|t| {
            let lib = normalize_for_match(&format!(
                "{} {}",
                t.title.as_deref().unwrap_or(""),
                t.artist.as_deref().unwrap_or("")
            ));
            let score = bigram_similarity(&query, &lib);
            (t, score)
        })
        .filter(|(_, score)| *score >= 0.25)
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let Some(track) = scored.first().map(|(t, _)| *t) else {
        return Ok(PerformTrackMatch::miss(String::new()));
    };

    let filename = track.source_filename.clone().unwrap_or_else(|| {
        track
            .file_path
            .split('/')
            .next_back()
            .unwrap_or("")
            .to_string()
    });

    let scores = crate::database::local::scores::get_scores_for_track(&mut access, &track.id)
        .await
        .map_err(CommandError::Internal)?;

    Ok(PerformTrackMatch {
        track_id: Some(track.id.clone()),
        has_annotations: !scores.is_empty(),
        filename,
    })
}

// ── BPM + fuzzy matching helpers ──────────────────────────────────────────────

/// Whether `lib_bpm` is within 5% of `src_bpm` at any of the ×1 / ×2 / ÷2
/// harmonics analyzers disagree on. Missing BPM on either side passes through
/// rather than filtering the candidate out.
fn bpm_matches(lib_bpm: f64, src_bpm: f64) -> bool {
    if lib_bpm <= 0.0 || src_bpm <= 0.0 {
        return true;
    }
    let tolerance = 0.05;
    [1.0f64, 2.0, 0.5]
        .iter()
        .any(|ratio| (lib_bpm * ratio - src_bpm).abs() / src_bpm <= tolerance)
}

/// Lowercase, drop everything non-alphanumeric, collapse whitespace.
fn normalize_for_match(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Character-bigram Jaccard similarity in [0, 1].
fn bigram_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let bigrams_a: HashSet<(char, char)> = a.chars().zip(a.chars().skip(1)).collect();
    let bigrams_b: HashSet<(char, char)> = b.chars().zip(b.chars().skip(1)).collect();
    if bigrams_a.is_empty() || bigrams_b.is_empty() {
        return 0.0;
    }
    let intersection = bigrams_a.intersection(&bigrams_b).count();
    let union = bigrams_a.len() + bigrams_b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

// ── Composite deck ────────────────────────────────────────────────────────────

/// Composite a track's light show and assign the result to a perform deck.
///
/// Two-step by design: compositing installs into `active_scene`, which is then
/// promoted to the deck slot — so this transiently clobbers whatever the track
/// editor had installed.
///
/// **Smell.** A deck has matched a *track*, not a score, so this still blends
/// every score on the `(track, venue)` — the ambiguity the editor's stage no
/// longer has. Which score a deck should play is a product question nobody has
/// answered; when it is, this becomes `install_score_scene` like every other
/// caller.
pub async fn render_composite_deck(
    services: &AppServices,
    deck_id: u8,
    track_id: String,
    venue_id: String,
) -> Result<(), CommandError> {
    let clips = crate::compositor::scores_for_track(&services.db.0, &venue_id, &track_id).await?;
    crate::compositor::install_track_scene(
        &services.db.0,
        &services.storage,
        &services.fixtures_root,
        &services.render_engine,
        &track_id,
        &venue_id,
        clips,
    )
    .await?;
    services.render_engine.promote_active_scene_to_deck(deck_id);
    Ok(())
}

/// Compile MIDI cues for a deck whose track has no match in the library.
///
/// The deck's live BPM and beat-in-bar stand in for a real beat grid, so
/// beat-reactive cues stay in phase with music Luma has never analyzed.
/// `beat_number` is 1-indexed (1–4 in 4/4); `position_secs` is the playback
/// position at the moment the track was loaded.
#[allow(clippy::too_many_arguments)]
pub async fn render_composite_deck_unmatched(
    services: &AppServices,
    deck_id: u8,
    bpm: f64,
    beat_number: u8,
    position_secs: f64,
    duration_secs: f64,
    venue_id: String,
) -> Result<(), CommandError> {
    let pool = &services.db.0;
    let _access = VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
    Ok(
        crate::controller_compositor::compile_cues_for_unmatched_deck(
            pool,
            &services.storage,
            Some(services.fixtures_root.clone()),
            &services.render_engine,
            deck_id,
            bpm as f32,
            beat_number,
            position_secs as f32,
            duration_secs as f32,
            &venue_id,
        )
        .await?,
    )
}
