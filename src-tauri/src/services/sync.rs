use crate::models::patterns::PatternSummary;
use crate::models::tracks::TrackSummary;
use crate::models::venues::Venue;
use reqwest::Client;
use serde::Serialize;

const SUPABASE_URL: &str = "https://smuuycypmsutwrkpctws.supabase.co";
const SUPABASE_ANON_KEY: &str = "sb_publishable_V8JRQkGliRYDAiGghjUrmQ_w8fpfjRb";

#[derive(Serialize)]
struct SyncVenue<'a> {
    id: &'a str, // Maps to remote_id
    uid: &'a str,
    name: &'a str,
    description: Option<&'a str>,
}

#[derive(Serialize)]
struct SyncPattern<'a> {
    id: &'a str,
    uid: &'a str,
    name: &'a str,
    description: Option<&'a str>,
    category_id: Option<i64>, // This might fail if category IDs are not synced/aligned. For now, sending as is.
}

#[derive(Serialize)]
struct SyncTrack<'a> {
    id: &'a str,
    uid: &'a str,
    track_hash: &'a str,
    title: Option<&'a str>,
    artist: Option<&'a str>,
    album: Option<&'a str>,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    duration_seconds: Option<f64>,
}

pub async fn push_venue(venue: &Venue, access_token: &str) -> Result<(), String> {
    let Some(remote_id) = &venue.remote_id else {
        return Ok(()); // Should not happen with new logic, but safe guard
    };
    let Some(uid) = &venue.uid else {
        return Ok(());
    };

    let payload = SyncVenue {
        id: remote_id,
        uid,
        name: &venue.name,
        description: venue.description.as_deref(),
    };

    push_to_supabase("venues", &payload, access_token).await
}

pub async fn push_pattern(pattern: &PatternSummary, access_token: &str) -> Result<(), String> {
    let Some(remote_id) = &pattern.remote_id else {
        return Ok(());
    };
    let Some(uid) = &pattern.uid else {
        return Ok(());
    };

    let payload = SyncPattern {
        id: remote_id,
        uid,
        name: &pattern.name,
        description: pattern.description.as_deref(),
        category_id: pattern.category_id,
    };

    push_to_supabase("patterns", &payload, access_token).await
}

pub async fn push_track(track: &TrackSummary, access_token: &str) -> Result<(), String> {
    let Some(remote_id) = &track.remote_id else {
        return Ok(());
    };
    let Some(uid) = &track.uid else {
        return Ok(());
    };

    let payload = SyncTrack {
        id: remote_id,
        uid,
        track_hash: &track.track_hash,
        title: track.title.as_deref(),
        artist: track.artist.as_deref(),
        album: track.album.as_deref(),
        track_number: track.track_number,
        disc_number: track.disc_number,
        duration_seconds: track.duration_seconds,
    };

    push_to_supabase("tracks", &payload, access_token).await
}

async fn push_to_supabase<T: Serialize>(
    table: &str,
    payload: &T,
    access_token: &str,
) -> Result<(), String> {
    let client = Client::new();
    let url = format!("{}/rest/v1/{}", SUPABASE_URL, table);

    // Upsert using POST with Prefer: resolution=merge-duplicates
    let res = client
        .post(&url)
        .header("apikey", SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .header("Prefer", "resolution=merge-duplicates")
        .json(payload)
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("Supabase error {}: {}", status, text));
    }

    Ok(())
}
