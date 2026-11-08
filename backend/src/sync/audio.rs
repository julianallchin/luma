//! On-demand audio downloads shared by playback, waveforms and analysis.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use sqlx::SqlitePool;
use tokio::sync::Mutex;

use super::error::SyncError;
use crate::database::local::auth;
use crate::database::local::track_access::{Operate, Read, VisibleTrackAccess};
use crate::database::local::write_admission::{enter_remote_writes, leave_remote_writes};
use crate::storage::StorageRoot;
use crate::sync::supabase::Storage;

pub(crate) struct TrackAudio {
    pool: SqlitePool,
    state_pool: SqlitePool,
    storage: StorageRoot,
    remote: Storage,
    downloads: Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

#[derive(sqlx::FromRow, PartialEq)]
struct AudioSource {
    uid: Option<String>,
    file_path: String,
    track_hash: String,
    storage_path: Option<String>,
}

impl TrackAudio {
    pub(crate) fn new(pool: SqlitePool, state_pool: SqlitePool, storage: StorageRoot) -> Self {
        Self {
            pool,
            state_pool,
            storage,
            remote: Storage::from_config(),
            downloads: Mutex::default(),
        }
    }

    /// Resolve local audio first, fetching and retaining missing audio once.
    /// Returns where the file is.
    pub(crate) async fn ensure(&self, track_id: &str) -> Result<PathBuf, SyncError> {
        let lock = {
            let mut downloads = self.downloads.lock().await;
            downloads.retain(|_, lock| lock.strong_count() > 0);
            let entry = downloads.entry(track_id.to_owned()).or_default();
            match entry.upgrade() {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    *entry = Arc::downgrade(&lock);
                    lock
                }
            }
        };
        let _download = lock.lock().await;
        let mut access = VisibleTrackAccess::<Read>::read(&self.pool, track_id)
            .await
            .map_err(SyncError::Local)?;
        let principal = access.principal().map(str::to_owned);
        let source: AudioSource = sqlx::query_as(
            "SELECT uid, file_path, track_hash, storage_path FROM tracks WHERE id = ?",
        )
        .bind(track_id)
        .fetch_one(access.connection())
        .await?;
        access.finish().await.map_err(SyncError::Local)?;
        if !source.file_path.ends_with(".stub") && Path::new(&source.file_path).is_file() {
            return Ok(source.file_path.into());
        }

        let storage_path = source.storage_path.as_deref().ok_or_else(|| {
            SyncError::Media("This track's audio has not been uploaded yet. Open Luma on the device that imported it to upload it.".into())
        })?;
        let (bucket, path) = storage_path
            .split_once('/')
            .filter(|(bucket, path)| !bucket.is_empty() && !path.is_empty())
            .ok_or_else(|| SyncError::Media("Invalid audio storage path".into()))?;
        let auth = auth::get_current_auth(&self.state_pool)
            .await?
            .ok_or(SyncError::AuthRequired)?;
        if principal.as_deref() != Some(auth.principal.user_id.as_str()) {
            return Err(SyncError::AuthRequired);
        }
        // Synced metadata must stay a filename inside the managed tracks directory.
        if source.track_hash.is_empty()
            || !source
                .track_hash
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(SyncError::Media("Invalid audio cache key".into()));
        }
        let ext = Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .filter(|ext| !ext.is_empty() && ext.bytes().all(|c| c.is_ascii_alphanumeric()))
            .ok_or_else(|| SyncError::Media("Invalid audio file extension".into()))?;
        let directory = self.storage.tracks_dir();
        let dest = directory.join(format!("{}.{ext}", source.track_hash));
        let bytes = self
            .remote
            .download_file(bucket, path, &auth.access_token)
            .await
            .map_err(|error| {
                if !error.is_missing_object() {
                    error
                } else if source.uid.as_deref() == Some(auth.principal.user_id.as_str()) {
                    SyncError::Media("This song's audio is missing from cloud storage. Import it again on a device that has the file.".into())
                } else {
                    SyncError::Media("This song was added by another member of this venue, and cloud storage isn't sharing its audio with you.".into())
                }
            })?;
        if bytes.is_empty() {
            return Err(SyncError::Media("Downloaded audio is empty".into()));
        }
        std::fs::create_dir_all(&directory).map_err(|e| SyncError::Media(e.to_string()))?;
        let mut temp = tempfile::NamedTempFile::new_in(&directory)
            .map_err(|e| SyncError::Media(e.to_string()))?;
        temp.write_all(&bytes)
            .map_err(|e| SyncError::Media(e.to_string()))?;

        // Do not publish a download across a track edit, deletion or account switch.
        let mut access = VisibleTrackAccess::<Operate>::operate(&self.pool, track_id)
            .await
            .map_err(SyncError::Local)?;
        if access.principal() != principal.as_deref() {
            return Err(SyncError::AuthRequired);
        }
        let current: AudioSource = sqlx::query_as(
            "SELECT uid, file_path, track_hash, storage_path FROM tracks WHERE id = ?",
        )
        .bind(track_id)
        .fetch_one(access.connection())
        .await?;
        if current != source {
            return Err(SyncError::Media(
                "Track changed while downloading audio; reopen it to retry".into(),
            ));
        }
        temp.persist(&dest)
            .map_err(|e| SyncError::Media(e.to_string()))?;
        // Shared tracks can belong to another account. This only updates the
        // device-local path, under the same admission used for downloaded stems.
        enter_remote_writes(access.connection())
            .await
            .map_err(SyncError::Local)?;
        sqlx::query("UPDATE tracks SET file_path = ? WHERE id = ?")
            .bind(dest.to_string_lossy().as_ref())
            .bind(track_id)
            .execute(access.connection())
            .await?;
        leave_remote_writes(access.connection())
            .await
            .map_err(SyncError::Local)?;
        access.commit().await.map_err(SyncError::Local)?;
        Ok(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::local::{database, state};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Server {
        url: String,
        requests: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn server(status: u16) -> Server {
        replying(status, "audio").await
    }

    async fn replying(status: u16, body: &'static str) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(
                    request.starts_with("GET /storage/v1/object/track-audio/alice/hash/audio.ogg ")
                );
                assert!(request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer "));
                count.fetch_add(1, Ordering::SeqCst);
                let reply = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        Server {
            url,
            requests,
            task,
        }
    }

    async fn fixture(directory: &Path, server: &Server) -> TrackAudio {
        fixture_as(directory, server, "alice").await
    }

    /// Alice's track in Alice's venue, which `user` is a member of when it is
    /// not Alice.
    async fn fixture_as(directory: &Path, server: &Server, user: &str) -> TrackAudio {
        let pool = database::init_app_db_at(directory).await.unwrap().0;
        let state_pool = state::init_state_db_at(directory).await.unwrap().0;
        sqlx::query("INSERT INTO tracks (id, uid, track_hash, title, file_path, storage_path) VALUES ('track', 'alice', 'hash', 'Remote track', 'hash.stub', 'track-audio/alice/hash/audio.ogg')")
            .execute(&pool).await.unwrap();
        if user != "alice" {
            sqlx::query(
                "INSERT INTO venues (id, uid, name) VALUES ('venue', 'alice', 'Room');
                 INSERT INTO scores (id, uid, track_id, venue_id, name)
                     VALUES ('score', 'alice', 'track', 'venue', 'Score');
                 INSERT INTO venue_members (id, uid, venue_id, role)
                     VALUES ('member', ?1, 'venue', 'member');",
            )
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
        }
        auth::install_test_session(&state_pool, user).await;
        auth::arm_write_admission(&pool, Some(user)).await.unwrap();
        let mut audio = TrackAudio::new(
            pool,
            state_pool,
            StorageRoot::from_path(directory.to_owned()),
        );
        audio.remote = Storage::new(server.url.clone(), "test-key".into());
        audio
    }

    async fn path(audio: &TrackAudio) -> String {
        sqlx::query_scalar("SELECT file_path FROM tracks WHERE id = 'track'")
            .fetch_one(&audio.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn concurrent_open_downloads_once_and_reopening_reuses_disk_offline() {
        let server = server(200).await;
        let directory = tempfile::tempdir().unwrap();
        let audio = fixture(directory.path(), &server).await;
        let (waveform, playback) = tokio::join!(audio.ensure("track"), audio.ensure("track"));
        waveform.unwrap();
        playback.unwrap();
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        let cached = path(&audio).await;
        assert_eq!(std::fs::read(&cached).unwrap(), b"audio");
        assert!(Path::new(&cached).starts_with(audio.storage.tracks_dir()));
        drop(server);
        let restarted = TrackAudio::new(
            audio.pool.clone(),
            audio.state_pool.clone(),
            audio.storage.clone(),
        );
        // No session or remote server is needed once the bytes are local.
        sqlx::query("DELETE FROM auth_session")
            .execute(&audio.state_pool)
            .await
            .unwrap();
        restarted.ensure("track").await.unwrap();
    }

    #[tokio::test]
    async fn failed_download_preserves_stub_and_can_retry() {
        let failed = server(404).await;
        let directory = tempfile::tempdir().unwrap();
        let mut audio = fixture(directory.path(), &failed).await;
        assert!(audio
            .ensure("track")
            .await
            .unwrap_err()
            .to_string()
            .contains("missing from cloud storage"));
        assert_eq!(path(&audio).await, "hash.stub");
        assert!(!audio.storage.tracks_dir().join("hash.ogg").exists());
        let success = server(200).await;
        audio.remote = Storage::new(success.url.clone(), "test-key".into());
        audio.ensure("track").await.unwrap();
        assert_eq!(std::fs::read(path(&audio).await).unwrap(), b"audio");
    }

    /// Storage's answer for an object that is not there, or that its read
    /// policy hides: a 400 whose body says 404.
    const NO_SUCH_KEY: &str = r#"{"statusCode":"404","error":"not_found","message":"Object not found","code":"NoSuchKey"}"#;

    #[tokio::test]
    async fn a_shared_track_storage_will_not_serve_says_so_in_words() {
        let server = replying(400, NO_SUCH_KEY).await;
        let directory = tempfile::tempdir().unwrap();
        let audio = fixture_as(directory.path(), &server, "bob").await;
        let message = audio.ensure("track").await.unwrap_err().to_string();
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        assert!(message.contains("another member"), "{message}");
        assert!(!message.contains("NoSuchKey"), "{message}");
        assert_eq!(path(&audio).await, "hash.stub");
    }

    #[tokio::test]
    async fn an_own_track_missing_from_storage_says_so_in_words() {
        let server = replying(400, NO_SUCH_KEY).await;
        let directory = tempfile::tempdir().unwrap();
        let audio = fixture(directory.path(), &server).await;
        let message = audio.ensure("track").await.unwrap_err().to_string();
        assert!(message.contains("missing from cloud storage"), "{message}");
        assert!(!message.contains("NoSuchKey"), "{message}");
    }

    #[tokio::test]
    async fn missing_upload_and_invisible_tracks_do_not_fetch() {
        let server = server(200).await;
        let directory = tempfile::tempdir().unwrap();
        let audio = fixture(directory.path(), &server).await;
        sqlx::query("UPDATE tracks SET storage_path = NULL WHERE id = 'track'")
            .execute(&audio.pool)
            .await
            .unwrap();
        assert!(audio
            .ensure("track")
            .await
            .unwrap_err()
            .to_string()
            .contains("not been uploaded"));
        auth::arm_write_admission(&audio.pool, Some("bob"))
            .await
            .unwrap();
        assert!(audio
            .ensure("track")
            .await
            .unwrap_err()
            .to_string()
            .contains("Track not found"));
        assert_eq!(server.requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn missing_local_file_is_downloaded_even_without_a_stub_path() {
        let server = server(200).await;
        let directory = tempfile::tempdir().unwrap();
        let audio = fixture(directory.path(), &server).await;
        sqlx::query("UPDATE tracks SET file_path = '/missing/import.mp3' WHERE id = 'track'")
            .execute(&audio.pool)
            .await
            .unwrap();
        audio.ensure("track").await.unwrap();
        assert_eq!(std::fs::read(path(&audio).await).unwrap(), b"audio");
    }

    #[tokio::test]
    async fn account_switch_during_download_does_not_publish_audio() {
        let server = server(200).await;
        let directory = tempfile::tempdir().unwrap();
        let mut audio = fixture(directory.path(), &server).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        audio.remote = Storage::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "test-key".into(),
        );
        let switch = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            auth::arm_write_admission(&audio.pool, Some("bob"))
                .await
                .unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\naudio",
                )
                .await
                .unwrap();
        };
        let (result, ()) = tokio::join!(audio.ensure("track"), switch);
        assert!(result.is_err());
        assert_eq!(path(&audio).await, "hash.stub");
        assert!(!audio.storage.tracks_dir().join("hash.ogg").exists());
    }
}
