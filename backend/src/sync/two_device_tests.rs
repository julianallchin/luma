//! Two app databases, one account, one PowerSync instance.
//!
//! This is the only test that can prove the client layer is right: raw-table
//! statements, upload triggers and the PostgREST translation only agree with
//! each other through a real server. `experiments/powersync/run.py` brings up
//! disposable Postgres, PostgREST and PowerSync containers and exports the
//! three variables below; without them the test skips.
//!
//! Ignored by default because it needs those containers:
//!
//! ```text
//! python experiments/powersync/run.py -- \
//!   cargo +1.97.1 test --manifest-path backend/Cargo.toml -p luma --lib \
//!   sync::two_device_tests -- --ignored
//! ```

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use luma_sync::powersync::{
    sdk::{error::PowerSyncError, BackendConnector, PowerSyncCredentials, SyncOptions, UpdateType},
    Connections, Database, HttpClient,
};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::schema::schema;
use super::triggers::install;

const USER: &str = "00000000-0000-0000-0000-0000000000aa";
const CONVERGE: Duration = Duration::from_secs(30);

struct Containers {
    powersync: String,
    postgrest: String,
    secret: String,
}

impl Containers {
    /// The environment `experiments/powersync/run.py` exports, or nothing.
    fn from_env() -> Option<Self> {
        Some(Self {
            powersync: std::env::var("LUMA_TEST_POWERSYNC_URL").ok()?,
            postgrest: std::env::var("LUMA_TEST_POSTGREST_URL").ok()?,
            secret: std::env::var("LUMA_TEST_JWT_SECRET").ok()?,
        })
    }
}

/// HMAC-SHA256, by hand. One test does not justify a dependency, and the
/// construction is five lines: pad the key to the block size, hash the inner
/// message, hash the outer.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let inner: Vec<u8> = block.iter().map(|byte| byte ^ 0x36).collect();
    let outer: Vec<u8> = block.iter().map(|byte| byte ^ 0x5c).collect();
    let mut hash = Sha256::new();
    hash.update(&inner);
    hash.update(message);
    let digest = hash.finalize();
    let mut hash = Sha256::new();
    hash.update(&outer);
    hash.update(digest);
    hash.finalize().into()
}

/// The Supabase-shaped access token `run.py`'s `mint_jwt` produces.
fn mint_jwt(secret: &str, user: &str) -> String {
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
        + 3600;
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "sub": user,
            "role": "authenticated",
            "aud": "authenticated",
            "iss": "supabase",
            "exp": expires,
        })
        .to_string(),
    );
    let signed = format!("{header}.{claims}");
    let signature = URL_SAFE_NO_PAD.encode(hmac_sha256(secret.as_bytes(), signed.as_bytes()));
    format!("{signed}.{signature}")
}

/// The production connector without the session database: a test mints its own
/// token rather than driving a Supabase sign-in. The upload translation is the
/// same, and it is what this test is here to check.
struct TestConnector {
    db: luma_sync::powersync::sdk::PowerSyncDatabase,
    http: reqwest::Client,
    postgrest: String,
    powersync: String,
    token: String,
}

#[async_trait::async_trait]
impl BackendConnector for TestConnector {
    async fn fetch_credentials(&self) -> Result<PowerSyncCredentials, PowerSyncError> {
        Ok(PowerSyncCredentials {
            endpoint: self.powersync.clone(),
            token: self.token.clone(),
        })
    }

    async fn upload_data(&self) -> Result<(), PowerSyncError> {
        while let Some(transaction) = self.db.next_crud_transaction().await? {
            for entry in &transaction.crud {
                let url = format!("{}/{}", self.postgrest.trim_end_matches('/'), entry.table);
                let filter = [("id", format!("eq.{}", entry.id))];
                let mut data = entry.data.clone().unwrap_or_default();
                let request = match entry.update_type {
                    UpdateType::Put => {
                        data.insert("id".into(), serde_json::Value::String(entry.id.clone()));
                        self.http
                            .post(&url)
                            .header("Prefer", "resolution=merge-duplicates,return=minimal")
                            .json(&serde_json::Value::Array(vec![serde_json::Value::Object(
                                data,
                            )]))
                    }
                    UpdateType::Patch => self
                        .http
                        .patch(&url)
                        .query(&filter)
                        .json(&serde_json::Value::Object(data)),
                    UpdateType::Delete => self.http.delete(&url).query(&filter),
                };
                let response = request
                    .header("apikey", &self.token)
                    .header("Authorization", format!("Bearer {}", self.token))
                    .send()
                    .await
                    .map_err(PowerSyncError::upload_error)?;
                assert!(
                    response.status().is_success(),
                    "{} {} {}: {}",
                    entry.table,
                    entry.id,
                    response.status(),
                    response.text().await.unwrap_or_default()
                );
            }
            transaction.complete().await?;
        }
        Ok(())
    }
}

/// One app database: migrations first on a plain pool, then the shared-file
/// pool pair with the upload triggers on every writer.
async fn device(path: &Path, containers: &Containers) -> Database {
    let migrate = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .expect("migration pool");
    sqlx::migrate!("./migrations")
        .run(&migrate)
        .await
        .expect("migrations");
    migrate.close().await;

    let connections = Connections::open_with(path, 4, |connection| {
        Box::pin(async move {
            install(connection)
                .await
                .map_err(|error| sqlx::Error::Configuration(error.into()))
        })
    })
    .await
    .expect("connections");
    crate::database::local::auth::arm_write_admission(connections.sql(), Some(USER))
        .await
        .expect("write admission");
    let database = connections
        .start(schema(), HttpClient::new())
        .await
        .expect("powersync");
    let connector = TestConnector {
        db: database.sync.clone(),
        http: reqwest::Client::new(),
        postgrest: containers.postgrest.clone(),
        powersync: containers.powersync.clone(),
        token: mint_jwt(&containers.secret, USER),
    };
    database.sync.connect(SyncOptions::new(connector)).await;
    database
}

/// Poll `query` on `pool` until it answers `expected`, or give up.
async fn settles(pool: &SqlitePool, query: &str, expected: &str, what: &str) {
    let deadline = Instant::now() + CONVERGE;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = sqlx::query_scalar::<_, Option<String>>(sqlx::AssertSqlSafe(query.to_owned()))
            .fetch_one(pool)
            .await
            .expect("query")
            .unwrap_or_default();
        if last == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("{what} never converged: saw {last:?}, wanted {expected:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see experiments/powersync/run.py"]
async fn powersync_two_devices() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a.db"), &containers).await;
    let b = device(&directory.path().join("b.db"), &containers).await;

    // A: a venue, a score, two clips.
    sqlx::query(
        "INSERT INTO venues (id, uid, name, role, environment, groups_initialized, created_at, updated_at)
         VALUES ('venue-1', ?, 'Test Room', 'owner', 'club', 0, 'now', 'now')",
    )
    .bind(USER)
    .execute(&a.sql)
    .await
    .expect("venue");
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name, created_at, updated_at)
         VALUES ('score-1', ?, 'track-1', 'venue-1', 'Opening', 'now', 'now')",
    )
    .bind(USER)
    .execute(&a.sql)
    .await
    .expect("score");
    for (id, start) in [("clip-1", 0.0), ("clip-2", 8.0)] {
        sqlx::query(
            "INSERT INTO clips (id, uid, score_id, graph, start, duration, seed, selection_json,
                 z_index, blend_mode, inputs_json, created_at, updated_at)
             VALUES (?, ?, 'score-1', '{}', ?, 8.0, '1', '[]', 0, 'normal', '{}', 'now', 'now')",
        )
        .bind(id)
        .bind(USER)
        .bind(start)
        .execute(&a.sql)
        .await
        .expect("clip");
    }

    settles(
        &b.sql,
        "SELECT CAST(COUNT(*) AS TEXT) FROM clips WHERE score_id = 'score-1'",
        "2",
        "B did not receive A's clips",
    )
    .await;

    // B edits a clip; A deletes the other.
    sqlx::query("UPDATE clips SET duration = 16.0, updated_at = 'edited' WHERE id = 'clip-1'")
        .execute(&b.sql)
        .await
        .expect("edit");
    sqlx::query("DELETE FROM clips WHERE id = 'clip-2'")
        .execute(&a.sql)
        .await
        .expect("delete");

    for (name, pool) in [("A", &a.sql), ("B", &b.sql)] {
        settles(
            pool,
            "SELECT group_concat(id || '=' || CAST(duration AS TEXT), ',')
               FROM (SELECT id, duration FROM clips WHERE score_id = 'score-1' ORDER BY id)",
            "clip-1=16.0",
            &format!("{name} did not converge on the edit and the delete"),
        )
        .await;
    }

    a.close().await;
    b.close().await;
}
