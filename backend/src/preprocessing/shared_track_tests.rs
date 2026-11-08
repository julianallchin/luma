//! A venue member opening a song another member owns: every derived artifact
//! this device computes for it, and every one it leaves to the owner.

use std::path::Path;

use crate::database::local::write_admission::{enter_remote_writes, leave_remote_writes};
use crate::dispatch::AppServices;
use crate::preprocessing::preprocessor::writes_synced_rows;
use crate::preprocessing::{registry, scheduler};
use crate::test_support::{signed_in, TEST_OWNER};

const OWNER: &str = "aaaaaaaa-0000-0000-0000-000000000000";
const RATE: u32 = crate::audio::SAMPLE_RATE;

/// One second of a 220 Hz tone, 16-bit stereo WAV at the decode rate.
fn wav() -> Vec<u8> {
    let mut samples = Vec::with_capacity(RATE as usize * 4);
    for frame in 0..RATE {
        let phase = f64::from(frame) / f64::from(RATE) * 220. * std::f64::consts::TAU;
        let value = (phase.sin() * 0.5 * f64::from(i16::MAX)) as i16;
        samples.extend_from_slice(&value.to_le_bytes());
        samples.extend_from_slice(&value.to_le_bytes());
    }
    let mut file = Vec::with_capacity(samples.len() + 44);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&2u16.to_le_bytes());
    file.extend_from_slice(&RATE.to_le_bytes());
    file.extend_from_slice(&(RATE * 4).to_le_bytes());
    file.extend_from_slice(&4u16.to_le_bytes());
    file.extend_from_slice(&16u16.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    file.extend_from_slice(&samples);
    file
}

/// Signed in as [`TEST_OWNER`], a member of [`OWNER`]'s venue, holding the
/// audio of `OWNER`'s track `shared` (downloaded, as the member's device does)
/// and of `private`, an `OWNER` track in no venue the member can see. The rows
/// arrive the way a sync download writes them.
async fn member(directory: &Path) -> AppServices {
    let services = signed_in(directory, directory.to_path_buf()).await;
    let audio = directory.join("shared.wav");
    std::fs::write(&audio, wav()).unwrap();
    let mut download = services.db.0.begin().await.unwrap();
    crate::sync::triggers::local_only(&mut download)
        .await
        .unwrap();
    enter_remote_writes(&mut download).await.unwrap();
    sqlx::query(
        "INSERT INTO tracks (id, uid, track_hash, title, duration_seconds, file_path)
             VALUES ('shared', ?1, 'shared-hash', 'Shared', 1.0, ?2),
                    ('private', ?1, 'private-hash', 'Private', 1.0, ?2);
         INSERT INTO venues (id, uid, name) VALUES ('venue', ?1, 'Room');
         INSERT INTO scores (id, uid, track_id, venue_id, name)
             VALUES ('score', ?1, 'shared', 'venue', 'Score');
         INSERT INTO venue_members (id, uid, venue_id, role)
             VALUES ('member', ?3, 'venue', 'member');",
    )
    .bind(OWNER)
    .bind(audio.to_string_lossy().as_ref())
    .bind(TEST_OWNER)
    .execute(&mut *download)
    .await
    .unwrap();
    leave_remote_writes(&mut download).await.unwrap();
    crate::sync::triggers::uploading(&mut download)
        .await
        .unwrap();
    download.commit().await.unwrap();
    services
}

async fn count(services: &AppServices, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(&services.db.0)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_member_caches_a_shared_songs_waveform_and_leaves_its_synced_analysis_to_the_owner() {
    let directory = tempfile::tempdir().unwrap();
    let services = member(directory.path()).await;

    // Opening the song: the waveform is this device's cache, under the
    // track's own owner.
    crate::dispatch::dispatch(
        &services,
        "get_track_waveform",
        &serde_json::json!({"trackId": "shared"}),
    )
    .await
    .unwrap();
    let uid: Option<String> =
        sqlx::query_scalar("SELECT uid FROM track_waveforms WHERE track_id = 'shared'")
            .fetch_one(&services.db.0)
            .await
            .unwrap();
    assert_eq!(uid.as_deref(), Some(OWNER));

    // The analysis DAG, both as reconcile queues it and as an open runs it.
    let synced: Vec<_> = registry::dag()
        .preprocessors()
        .filter(|p| writes_synced_rows(p.as_ref()))
        .collect();
    assert!(!synced.is_empty());
    for p in &synced {
        assert!(
            p.list_pending(&services.db.0).await.unwrap().is_empty(),
            "{} queued another principal's track",
            p.name()
        );
    }
    services.reconcile_analysis().await.unwrap();
    let epoch = services.analysis_tasks.current_epoch().unwrap();
    let lease = services.analysis_tasks.lease(epoch).unwrap();
    scheduler::run_for_track(
        &services.db.0,
        &services.storage,
        &services.workers,
        "shared",
        &lease.guard(),
        None,
    )
    .await
    .unwrap();
    for p in &synced {
        let rows = count(
            &services,
            &format!(
                "SELECT COUNT(*) FROM {} WHERE track_id = 'shared'",
                p.artifact_table()
            ),
        )
        .await;
        assert_eq!(rows, 0, "{} wrote the owner's rows", p.name());
    }
    assert_eq!(
        count(&services, "SELECT COUNT(*) FROM preprocessing_failures").await,
        0,
        "waiting for the owner's analysis is not a failure"
    );

    // Not a door to tracks the member cannot see.
    assert!(sqlx::query(
        "INSERT INTO track_waveforms (track_id, uid, preview_samples_blob, band_gain_low, band_gain_mid, band_gain_high)
         VALUES ('private', ?, x'00', 1, 1, 1)",
    )
    .bind(OWNER)
    .execute(&services.db.0)
    .await
    .is_err());
    // Nor a way to stamp the cache with somebody other than the track's owner.
    assert!(
        sqlx::query("UPDATE track_waveforms SET uid = ? WHERE track_id = 'shared'")
            .bind(TEST_OWNER)
            .execute(&services.db.0)
            .await
            .is_err()
    );

    // When sync removes the shared song, its cached waveform goes with it.
    let mut download = services.db.0.begin().await.unwrap();
    crate::sync::triggers::local_only(&mut download)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM scores WHERE track_id = 'shared'; DELETE FROM tracks WHERE id = 'shared'",
    )
    .execute(&mut *download)
    .await
    .unwrap();
    crate::sync::triggers::uploading(&mut download)
        .await
        .unwrap();
    download.commit().await.unwrap();
    assert_eq!(
        count(&services, "SELECT COUNT(*) FROM track_waveforms").await,
        0
    );
}
