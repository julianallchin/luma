//! Python figures as files, not as transcript bytes.
//!
//! A stored figure names its PNG by `path` — `agent-figures/<uid>/<sha>.png`,
//! the same `bucket/object` form as a track's `storage_path`. The bytes sit in
//! a local cache keyed by their hash, and the media loop uploads them to the
//! bucket. Content-addressed, so a second upload of the same figure rewrites
//! the same object.

use std::path::PathBuf;

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::database::remote::common::SupabaseClient;
use crate::storage::StorageRoot;

/// The Storage bucket, and the first segment of every figure `path`.
pub const BUCKET: &str = "agent-figures";

/// A thread with no owner files its figures under this name in place of a
/// uid. Such a figure stays in the local cache and is never uploaded: no
/// session can write to a folder that no uid owns.
const UNOWNED: &str = "local";

/// Cache `png` and return its stored `path`. An owned figure is also queued
/// for upload.
///
/// # Errors
///
/// If the cache file or the queue row cannot be written.
pub async fn store(
    pool: &SqlitePool,
    storage: &StorageRoot,
    owner: Option<&str>,
    png: &[u8],
) -> Result<String, String> {
    let path = path(owner, &cache(storage, png)?);
    if owner.is_some() {
        sqlx::query("INSERT OR IGNORE INTO agent_figure_uploads (path) VALUES (?)")
            .bind(&path)
            .execute(pool)
            .await
            .map_err(|error| format!("queue figure upload: {error}"))?;
    }
    Ok(path)
}

/// Put `png` in the local cache, under its SHA-256, and return the hash.
///
/// # Errors
///
/// If the cache file cannot be written.
pub fn cache(storage: &StorageRoot, png: &[u8]) -> Result<String, String> {
    let sha = sha(png);
    let local = storage.agent_figure_path(&sha);
    if !local.is_file() {
        write_atomic(&local, png)?;
    }
    Ok(sha)
}

/// The hex SHA-256 a figure is named by.
#[must_use]
pub fn sha(png: &[u8]) -> String {
    format!("{:x}", Sha256::digest(png))
}

/// The stored path of the figure whose PNG hashes to `sha`.
#[must_use]
pub fn path(owner: Option<&str>, sha: &str) -> String {
    format!("{BUCKET}/{}/{sha}.png", owner.unwrap_or(UNOWNED))
}

/// Where a figure's bytes sit on this machine, whether or not they are there.
#[must_use]
pub fn cached(storage: &StorageRoot, path: &str) -> PathBuf {
    let sha = path
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".png"))
        .unwrap_or_default();
    storage.agent_figure_path(sha)
}

/// A figure's bytes: from the cache, or else downloaded with the signed-in
/// session and cached.
///
/// # Errors
///
/// If the figure is not cached and there is no session, or the download
/// fails.
pub async fn load(
    storage: &StorageRoot,
    state_pool: &SqlitePool,
    path: &str,
) -> Result<Vec<u8>, String> {
    let local = cached(storage, path);
    if let Ok(bytes) = std::fs::read(&local) {
        return Ok(bytes);
    }
    let token = crate::database::local::auth::get_current_access_token(state_pool)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "sign in to load this figure".to_string())?;
    let object = path
        .strip_prefix(&format!("{BUCKET}/"))
        .ok_or_else(|| format!("not a figure path: {path}"))?;
    let bytes = SupabaseClient::new(
        crate::config::SUPABASE_URL.to_owned(),
        crate::config::supabase_anon_key(),
    )
    .download_file(BUCKET, object, &token)
    .await
    .map_err(|error| error.to_string())?;
    write_atomic(&local, &bytes)?;
    Ok(bytes)
}

fn write_atomic(dest: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let write = || {
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = dest.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, dest)
    };
    write().map_err(|error| format!("write figure {}: {error}", dest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_figure_is_cached_by_its_hash_and_queued_only_when_owned() {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageRoot::from_path(dir.path().to_path_buf());
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE agent_figure_uploads (path TEXT PRIMARY KEY NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();

        let owned = store(&pool, &storage, Some("u1"), b"png").await.unwrap();
        let again = store(&pool, &storage, Some("u1"), b"png").await.unwrap();
        let guest = store(&pool, &storage, None, b"png").await.unwrap();

        assert_eq!(owned, again, "the same bytes name the same object");
        assert!(owned.starts_with("agent-figures/u1/") && owned.ends_with(".png"));
        assert_eq!(cached(&storage, &owned), cached(&storage, &guest));
        assert_eq!(std::fs::read(cached(&storage, &owned)).unwrap(), b"png");
        let queued: Vec<String> = sqlx::query_scalar("SELECT path FROM agent_figure_uploads")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(queued, vec![owned]);
    }
}
