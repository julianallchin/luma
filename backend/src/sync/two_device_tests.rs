//! Two app databases against one PowerSync instance.
//!
//! Raw-table statements, upload triggers and the PostgREST translation only
//! agree with each other through a real server, so these are the only tests
//! that can prove the client layer right. They need the containers
//! `experiments/powersync/run.py` brings up, and skip without them:
//!
//! ```text
//! python3 experiments/powersync/run.py --test sync::two_device_tests
//! ```

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use luma_sync::powersync::{
    sdk::{error::PowerSyncError, BackendConnector, PowerSyncCredentials, SyncOptions, UpdateType},
    Database, HttpClient,
};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::schema::schema;

/// The two accounts `experiments/powersync/run.py` puts in `auth.users`
/// before it runs these tests. Fixed rather than minted here because the
/// server's foreign keys are real: a row whose `uid` is nobody is refused.
const USER: &str = "00000000-0000-0000-0000-0000000000aa";
const OTHER: &str = "00000000-0000-0000-0000-0000000000bb";
/// A third account that joins nothing. Every row it can see or write is a hole
/// in the policies.
const STRANGER: &str = "00000000-0000-0000-0000-0000000000cc";
const CONVERGE: Duration = Duration::from_secs(30);

/// The key id in `experiments/powersync/service.yaml`'s inline JWKS.
const JWT_KID: &str = "luma-row-model-test";

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
    // The `kid` names the key in the service's inline JWKS — see
    // `experiments/powersync/service.yaml`. PowerSync refuses a token whose
    // header names no key it knows.
    let header = URL_SAFE_NO_PAD
        .encode(serde_json::json!({ "alg": "HS256", "kid": JWT_KID, "typ": "JWT" }).to_string());
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "sub": user,
            "role": "authenticated",
            "aud": "authenticated",
            "iat": expires - 3600,
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

/// One app database, opened exactly the way the app opens it: the real
/// migrations, the real trigger sets, the real PowerSync connection pair.
///
/// `directory` is an app config directory, so the file inside it is `luma.db`
/// and two devices are two directories.
async fn device(directory: &Path, containers: &Containers, user: &str) -> Database {
    let database = offline_device(directory, user).await;
    connect(&database, containers, user).await;
    database
}

/// The same database, not yet connected — what a device looks like on a plane.
async fn offline_device(directory: &Path, user: &str) -> Database {
    let (_db, connections) = crate::database::local::database::open_app_db_at(directory)
        .await
        .expect("app database");
    crate::database::local::auth::arm_write_admission(connections.sql(), Some(user))
        .await
        .expect("write admission");
    connections
        .start(schema(), HttpClient::new())
        .await
        .expect("powersync")
}

/// Point a device at the stack and start syncing.
async fn connect(database: &Database, containers: &Containers, user: &str) {
    let connector = TestConnector {
        db: database.sync.clone(),
        http: reqwest::Client::new(),
        postgrest: containers.postgrest.clone(),
        powersync: containers.powersync.clone(),
        token: mint_jwt(&containers.secret, user),
    };
    database.sync.connect(SyncOptions::new(connector)).await;
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
    panic!(
        "{what} never converged: saw {last:?}, wanted {expected:?}\n{}",
        inventory(pool).await
    );
}

/// What this device actually holds, for a failure that would otherwise only
/// say "nothing arrived".
async fn inventory(pool: &SqlitePool) -> String {
    let mut lines = Vec::new();
    for table in [
        "venues",
        "venue_members",
        "tracks",
        "scores",
        "clips",
        "changes",
        "drafts",
    ] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(pool)
                .await
                .unwrap_or(-1);
        lines.push(format!("{table}={count}"));
    }
    for (table, column) in [
        ("ps_crud", "id"),
        ("ps_buckets", "id"),
        ("ps_oplog", "bucket"),
    ] {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT({column}) FROM {table}"
        )))
        .fetch_one(pool)
        .await
        .unwrap_or(-1);
        lines.push(format!("{table}={count}"));
    }
    lines.push(refused(pool).await);
    format!("device holds: {}", lines.join(" "))
}

/// The first downloaded row the local schema refuses, if there is one.
///
/// A checkpoint applies every row in one transaction, so one bad row means
/// nothing arrives and the SDK only says `CONSTRAINT`. This replays the oplog
/// through the same generated statements to name the row.
async fn refused(pool: &SqlitePool) -> String {
    let rows: Vec<(String, String)> =
        match sqlx::query_as("SELECT row_type, data FROM ps_oplog WHERE data IS NOT NULL")
            .fetch_all(pool)
            .await
        {
            Ok(rows) => rows,
            Err(error) => return format!("oplog unreadable: {error}"),
        };
    // The SDK's own connection has foreign keys off, which is what makes a
    // checkpoint's arbitrary row order survivable. Match it, or every replay
    // fails on a parent that has not been written yet.
    let mut connection = match pool.acquire().await {
        Ok(connection) => connection,
        Err(error) => return format!("could not replay the oplog: {error}"),
    };
    let keys = std::env::var("LUMA_REPLAY_FK").unwrap_or_else(|_| "OFF".into());
    if let Err(error) = sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA foreign_keys = {keys}")))
        .execute(&mut *connection)
        .await
    {
        return format!("could not replay the oplog: {error}");
    }
    // One transaction, like a checkpoint: a row that only conflicts with
    // another row in the same batch would pass a one-at-a-time replay.
    let _ = sqlx::query("BEGIN").execute(&mut *connection).await;
    for (name, data) in rows {
        let Some(table) = super::schema::table(&name) else {
            continue;
        };
        let Ok(row) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data)
        else {
            continue;
        };
        let (put, _) = super::schema::statements(table);
        let mut query = sqlx::query(sqlx::AssertSqlSafe(put.clone()));
        for column in table.columns {
            query = query.bind(match row.get(*column) {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(text)) => Some(text.clone()),
                Some(value) => Some(value.to_string()),
            });
        }
        let id = row
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        for _ in 0..table
            .local_defaults
            .iter()
            .flat_map(|(_, fill)| fill.matches('?'))
            .count()
        {
            query = query.bind(Some(id.to_owned()));
        }
        if let Err(error) = query.execute(&mut *connection).await {
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            return format!("refused: {name} {id}: {error}");
        }
    }
    if let Err(error) = sqlx::query("COMMIT").execute(&mut *connection).await {
        let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
        return format!("refused at commit ({keys}): {error}");
    }
    let _ = sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await;
    "refused: nothing".to_owned()
}

/// Poll `query` on `pool` until `predicate` accepts the answer.
async fn until(
    pool: &SqlitePool,
    query: &str,
    what: &str,
    predicate: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + CONVERGE;
    let mut last;
    loop {
        last = sqlx::query_scalar::<_, Option<String>>(sqlx::AssertSqlSafe(query.to_owned()))
            .fetch_one(pool)
            .await
            .expect("query")
            .unwrap_or_default();
        if predicate(&last) {
            return last;
        }
        if Instant::now() >= deadline {
            panic!("{what} never converged: saw {last:?}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A track, a venue, a score, and `clips` clips on it — the smallest library
/// with something in it. Written with the real trigger sets, so every row here
/// is also an upload.
async fn seed(pool: &SqlitePool, user: &str, venue: &str, score: &str, clips: &[(&str, f64)]) {
    sqlx::query(
        "INSERT INTO venues (id, uid, name, role, groups_initialized)
         VALUES (?, ?, 'Test Room', 'owner', 1)",
    )
    .bind(venue)
    .bind(user)
    .execute(pool)
    .await
    .expect("venue");
    let track = format!("{venue}-track");
    sqlx::query(
        "INSERT INTO tracks (id, uid, track_hash, title, file_path)
         VALUES (?, ?, ?, 'Aurora', '/nowhere.wav')",
    )
    .bind(&track)
    .bind(user)
    .bind(&track)
    .execute(pool)
    .await
    .expect("track");
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name)
         VALUES (?, ?, ?, ?, 'Opening')",
    )
    .bind(score)
    .bind(user)
    .bind(&track)
    .bind(venue)
    .execute(pool)
    .await
    .expect("score");
    for (id, start) in clips {
        clip(pool, user, score, id, *start, 8.0).await;
    }
}

async fn clip(pool: &SqlitePool, user: &str, score: &str, id: &str, start: f64, duration: f64) {
    sqlx::query(
        "INSERT INTO clips (id, uid, score_id, graph, start, duration, seed, selection_json,
             z_index, blend_mode, inputs_json)
         VALUES (?, ?, ?, 'strobe', ?, ?, '1', '{\"expression\":\"all\"}', 0, 'replace', '{}')",
    )
    .bind(format!("{score}:{id}"))
    .bind(user)
    .bind(score)
    .bind(start)
    .bind(duration)
    .execute(pool)
    .await
    .expect("clip");
}

/// The clips of one score, as `key=duration` pairs. What every convergence
/// assertion below compares.
///
/// Scoped to one score because the containers outlive one test run: two runs
/// against a kept stack share a database, and an unscoped count would be a
/// test of how many times it had been run.
fn clip_shape(score: &str) -> String {
    format!(
        "SELECT group_concat(key || '=' || CAST(duration AS TEXT), ',') FROM (
             SELECT replace(id, '{score}:', '') AS key, duration
             FROM clips WHERE score_id = '{score}' ORDER BY id)"
    )
}

/// How many rows of `table` belong to `scope`.
fn count(table: &str, column: &str, scope: &str) -> String {
    format!("SELECT CAST(COUNT(*) AS TEXT) FROM {table} WHERE {column} = '{scope}'")
}

/// Two devices, one account: a score written on A appears on B, each edits,
/// and both land on the same rows.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see experiments/powersync/run.py"]
async fn powersync_two_devices() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, USER).await;

    let venue = unique("venue");
    let score = unique("score");
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0), ("two", 8.0)]).await;

    settles(
        &b.sql,
        &count("clips", "score_id", &score),
        "2",
        "B did not receive A's clips",
    )
    .await;

    // B edits a clip; A deletes the other.
    sqlx::query("UPDATE clips SET duration = 16.0 WHERE id = ?")
        .bind(format!("{score}:one"))
        .execute(&b.sql)
        .await
        .expect("edit");
    sqlx::query("DELETE FROM clips WHERE id = ?")
        .bind(format!("{score}:two"))
        .execute(&a.sql)
        .await
        .expect("delete");

    for (name, pool) in [("A", &a.sql), ("B", &b.sql)] {
        settles(
            pool,
            &clip_shape(&score),
            "one=16.0",
            &format!("{name} did not converge on the edit and the delete"),
        )
        .await;
    }

    a.close().await;
    b.close().await;
}

/// An edit made with no network, then a restart, then a reconnect.
///
/// The upload queue is a table in the same SQLite file, so closing the database
/// and opening it again is the interesting part: if the queue did not survive
/// the restart, the edit would be lost in a way nothing else in this suite
/// would notice.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see experiments/powersync/run.py"]
async fn an_offline_edit_survives_a_restart() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let path = directory.path().join("b");
    let b = device(&path, &containers, USER).await;

    let venue = unique("venue");
    let score = unique("score");
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0)]).await;
    settles(
        &b.sql,
        &count("clips", "score_id", &score),
        "1",
        "B did not receive the clip",
    )
    .await;

    // B goes offline and edits anyway.
    b.sync.disconnect().await;
    sqlx::query("UPDATE clips SET duration = 32.0 WHERE id = ?")
        .bind(format!("{score}:one"))
        .execute(&b.sql)
        .await
        .expect("offline edit");
    let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ps_crud")
        .fetch_one(&b.sql)
        .await
        .expect("queue");
    assert!(queued > 0, "an offline edit left nothing to upload");

    // B restarts: the process is gone, the file is not.
    b.close().await;
    let b = device(&path, &containers, USER).await;

    for (name, pool) in [("A", &a.sql), ("B", &b.sql)] {
        settles(
            pool,
            &clip_shape(&score),
            "one=32.0",
            &format!("{name} did not see the edit B made offline"),
        )
        .await;
    }

    a.close().await;
    b.close().await;
}

/// Two users and a share code: B joins A's venue, sees the score, edits it, A
/// sees the edit — and when A revokes the membership B loses the venue's rows.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see experiments/powersync/run.py"]
async fn a_share_code_admits_a_second_user_until_it_is_revoked() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, OTHER).await;

    let venue = unique("venue");
    let score = unique("score");
    let code = unique("code");
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0)]).await;
    sqlx::query("UPDATE venues SET share_code = ? WHERE id = ?")
        .bind(&code)
        .bind(&venue)
        .execute(&a.sql)
        .await
        .expect("share code");
    // The code has to be on the server before anyone can redeem it.
    settles(
        &b.sql,
        &count("venues", "id", &venue),
        "0",
        "B could see the venue before joining",
    )
    .await;
    wait_for_upload(&a.sql).await;

    // The join is a server RPC: an ordinary client may not insert a membership
    // into a venue it cannot yet see.
    let joined = rpc_join(&containers, OTHER, &code).await;
    assert_eq!(joined, venue);

    settles(
        &b.sql,
        &count("clips", "score_id", &score),
        "1",
        "B joined but the venue's score never arrived",
    )
    .await;
    settles(
        &b.sql,
        &count("venue_members", "venue_id", &venue),
        "1",
        "B never received its own membership row",
    )
    .await;

    // A member edits venue content and the owner sees it.
    sqlx::query("UPDATE clips SET duration = 12.0 WHERE id = ?")
        .bind(format!("{score}:one"))
        .execute(&b.sql)
        .await
        .expect("member edit");
    settles(
        &a.sql,
        &clip_shape(&score),
        "one=12.0",
        "the owner never saw the member's edit",
    )
    .await;

    // Revocation is a delete of the membership row, and it reaches B as a
    // download that empties every bucket the membership put them in.
    sqlx::query("DELETE FROM venue_members WHERE venue_id = ?")
        .bind(&venue)
        .execute(&a.sql)
        .await
        .expect("revoke");
    settles(
        &b.sql,
        &count("venues", "id", &venue),
        "0",
        "B kept the venue after the membership was revoked",
    )
    .await;
    settles(
        &b.sql,
        &count("clips", "score_id", &score),
        "0",
        "B kept the venue's clips after the membership was revoked",
    )
    .await;

    a.close().await;
    b.close().await;
}

/// A subagent's draft, merged, arriving on the other device as clips.
///
/// The draft itself syncs — it is the agent's workspace and it follows its
/// owner — but what the other device shows is the merge: the live rows the
/// merge wrote.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see experiments/powersync/run.py"]
async fn a_merged_draft_arrives_as_clips() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, USER).await;

    let venue = unique("venue");
    let score = unique("score");
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0)]).await;
    settles(
        &b.sql,
        &count("clips", "score_id", &score),
        "1",
        "B did not receive the score",
    )
    .await;

    // The agent works on a copy of the score, never on the live rows.
    let thread = unique("thread");
    sqlx::query(
        "INSERT INTO agent_threads (id, uid, agent_kind, subject_kind, subject_id, score_id)
         VALUES (?, ?, 'track', 'score', ?, ?)",
    )
    .bind(&thread)
    .bind(USER)
    .bind(&score)
    .bind(&score)
    .execute(&a.sql)
    .await
    .expect("thread");
    let mut writer = a.sql.acquire().await.expect("writer");
    let draft = crate::services::drafts::create(&mut writer, &score, &thread, USER)
        .await
        .expect("draft");

    // What the subagent does: add a clip to its own copy.
    let mut state = crate::services::drafts::state(&mut writer, &draft)
        .await
        .expect("draft state");
    state.clips.insert(
        "two".into(),
        luma_patterns::Clip {
            graph: "strobe".into(),
            start: 8.0,
            duration: 4.0,
            seed: 7,
            selection_seed: None,
            selection: luma_patterns::Selection::all(),
            z_index: 0,
            blend_mode: luma_patterns::BlendMode::Replace,
            inputs: Default::default(),
        },
    );
    crate::services::drafts::apply(&mut writer, &draft, &state)
        .await
        .expect("draft apply");
    crate::services::drafts::merge(&mut writer, &draft)
        .await
        .expect("merge");
    drop(writer);

    settles(
        &b.sql,
        &clip_shape(&score),
        "one=8.0,two=4.0",
        "the merged draft never reached the other device",
    )
    .await;
    // The draft is gone on both: merge deletes it, and the delete uploads.
    settles(
        &b.sql,
        &count("drafts", "score_id", &score),
        "0",
        "the merged draft was not cleaned up on the other device",
    )
    .await;

    a.close().await;
    b.close().await;
}

/// A conversation follows its owner between devices, messages and all.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see experiments/powersync/run.py"]
async fn a_conversation_follows_its_owner() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, USER).await;

    let thread = unique("thread");
    sqlx::query(
        "INSERT INTO agent_threads (id, uid, agent_kind, subject_kind, subject_id, title)
         VALUES (?, ?, 'track', 'none', NULL, 'About the drop')",
    )
    .bind(&thread)
    .bind(USER)
    .execute(&a.sql)
    .await
    .expect("thread");
    let principal = crate::database::local::auth::principal_key(Some(USER));
    let mut parent: Option<String> = None;
    for (index, role) in ["user", "assistant"].into_iter().enumerate() {
        let id = format!("{thread}-m{index}");
        sqlx::query(
            "INSERT INTO agent_thread_messages
                 (id, uid, principal_key, created_in_thread_id, parent_message_id, depth, role,
                  parts_json, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, '[]',
                     strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(USER)
        .bind(&principal)
        .bind(&thread)
        .bind(parent.as_ref())
        .bind(i64::try_from(index).unwrap())
        .bind(role)
        .execute(&a.sql)
        .await
        .expect("message");
        parent = Some(id);
    }

    settles(
        &b.sql,
        &format!(
            "SELECT group_concat(role, ',') FROM (SELECT role FROM agent_thread_messages
                 WHERE created_in_thread_id = '{thread}' ORDER BY depth)"
        ),
        "user,assistant",
        "the conversation did not follow its owner",
    )
    .await;
    settles(
        &b.sql,
        &format!("SELECT title FROM agent_threads WHERE id = '{thread}'"),
        "About the drop",
        "the thread itself did not arrive",
    )
    .await;

    a.close().await;
    b.close().await;
}

/// The two shapes of access that are not a venue: the verified pattern library
/// reaches every signed-in account and only its owner may edit it, and a
/// person's private rows reach nobody else at all.
///
/// The pattern is written last, so B receiving it proves A's earlier uploads
/// have been replicated too — which is what makes the counts of zero below an
/// assertion rather than a race.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see experiments/powersync/run.py"]
async fn the_verified_library_is_public_and_private_rows_are_not() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, OTHER).await;

    let venue = unique("venue");
    let score = unique("score");
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0)]).await;
    let draft = unique("draft");
    sqlx::query(
        "INSERT INTO drafts (id, uid, score_id, base_json, state_json)
         VALUES (?, ?, ?, '{}', '{}')",
    )
    .bind(&draft)
    .bind(USER)
    .bind(&score)
    .execute(&a.sql)
    .await
    .expect("draft");
    let thread = unique("thread");
    sqlx::query(
        "INSERT INTO agent_threads (id, uid, agent_kind, subject_kind, subject_id)
         VALUES (?, ?, 'track', 'none', NULL)",
    )
    .bind(&thread)
    .bind(USER)
    .execute(&a.sql)
    .await
    .expect("thread");

    let pattern = unique("pattern");
    sqlx::query("INSERT INTO patterns (id, uid, name, is_verified) VALUES (?, ?, 'Strobe', 1)")
        .bind(&pattern)
        .bind(USER)
        .execute(&a.sql)
        .await
        .expect("pattern");
    wait_for_upload(&a.sql).await;

    settles(
        &b.sql,
        &count("patterns", "id", &pattern),
        "1",
        "the verified library never reached the other account",
    )
    .await;
    for (table, column, scope) in [
        ("drafts", "id", draft.as_str()),
        ("agent_threads", "id", thread.as_str()),
        ("changes", "row_id", score.as_str()),
        ("venues", "id", venue.as_str()),
    ] {
        let held: String = sqlx::query_scalar(sqlx::AssertSqlSafe(count(table, column, scope)))
            .fetch_one(&b.sql)
            .await
            .expect("query");
        assert_eq!(
            held, "0",
            "{table} reached an account it does not belong to"
        );
    }

    // Verified does not mean writable: the policy filters the row out of the
    // other account's update, which PostgREST reports as a success over zero
    // rows. The owner's name is what proves nothing was written.
    patch(
        &containers,
        OTHER,
        &format!("/patterns?id=eq.{pattern}"),
        &serde_json::json!({ "name": "Hijack" }),
    )
    .await;
    assert_eq!(
        get(
            &containers,
            USER,
            &format!("/patterns?id=eq.{pattern}&select=name")
        )
        .await,
        serde_json::json!([{ "name": "Strobe" }]),
        "another account rewrote a verified pattern"
    );

    a.close().await;
    b.close().await;
}

/// What a venue shares, and what it does not, from the far side of the wire.
///
/// The share-code test covers the member's happy path. These are the refusals
/// beside it: a stranger sees nothing of the venue, cannot write into it, and
/// cannot talk their way in with a code that is not the code. Asserted over
/// PostgREST rather than over a device, because a refusal is the server's
/// answer and a client that never asked would pass either way.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see experiments/powersync/run.py"]
async fn a_stranger_reaches_nothing_of_a_shared_venue() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, OTHER).await;

    let venue = unique("venue");
    let score = unique("score");
    let code = unique("code");
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0)]).await;
    let track = format!("{venue}-track");
    sqlx::query("UPDATE venues SET share_code = ? WHERE id = ?")
        .bind(&code)
        .bind(&venue)
        .execute(&a.sql)
        .await
        .expect("share code");
    wait_for_upload(&a.sql).await;

    for path in [
        format!("/venues?id=eq.{venue}&select=id"),
        format!("/tracks?id=eq.{track}&select=id"),
        format!("/clips?score_id=eq.{score}&select=id"),
    ] {
        assert_eq!(
            get(&containers, STRANGER, &path).await,
            serde_json::json!([]),
            "a stranger read {path}"
        );
    }

    let (status, body) = try_rest(
        &containers,
        STRANGER,
        reqwest::Method::POST,
        "/clips",
        Some(&serde_json::json!({
            "id": unique("clip"), "score_id": score, "graph": "strobe",
            "start": 0.0, "duration": 1.0, "seed": "1",
            "selection_json": "{\"expression\":\"all\"}",
        })),
    )
    .await;
    assert_eq!(
        status, 403,
        "a stranger wrote into the shared score: {body}"
    );

    let (status, body) = try_rest(
        &containers,
        STRANGER,
        reqwest::Method::POST,
        "/rpc/join_venue",
        Some(&serde_json::json!({ "code": format!("{code}-not") })),
    )
    .await;
    assert!(
        status.is_client_error() || status.is_server_error(),
        "a wrong share code joined the venue: {status} {body}"
    );

    // The member's side of the same policies, so a stranger seeing nothing is
    // not merely a venue nobody can reach.
    assert_eq!(rpc_join(&containers, OTHER, &code).await, venue);
    settles(
        &b.sql,
        &count("tracks", "id", &track),
        "1",
        "the track behind the shared score never reached the member",
    )
    .await;

    a.close().await;
    b.close().await;
}

/// One row of every venue-child shape, and all of it on the member's device.
///
/// This is what evaluates each query in `deploy/sync-rules.yaml` against real
/// data: a rule that names a column the table does not have, or a subquery the
/// service refuses, fails the whole stream, and the only symptom is rows that
/// never arrive.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see experiments/powersync/run.py"]
async fn every_venue_child_shape_reaches_a_member() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, OTHER).await;

    let venue = unique("venue");
    let score = unique("score");
    let code = unique("code");
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0)]).await;
    let track = format!("{venue}-track");
    let pattern = unique("pattern");
    let root = format!("{venue}:venue");
    let child = unique("node");
    let fixture = unique("fixture");
    let group = unique("group");
    for statement in [
        format!("UPDATE venues SET share_code = '{code}' WHERE id = '{venue}'"),
        format!("INSERT INTO patterns (id, uid, name) VALUES ('{pattern}', '{USER}', 'Strobe')"),
        format!(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES ('{pattern}-impl', '{USER}', '{pattern}', '{{}}')"
        ),
        format!(
            "INSERT INTO score_definitions (id, uid, score_id, definition_json)
             VALUES ('{score}:strobe', '{USER}', '{score}', '{{}}')"
        ),
        format!(
            "INSERT INTO track_beats (track_id, uid, beats_json, downbeats_json)
             VALUES ('{track}', '{USER}', '[]', '[]')"
        ),
        format!(
            "INSERT INTO venue_nodes (id, uid, venue_id, kind)
             VALUES ('{root}', '{USER}', '{venue}', 'venue'),
                    ('{child}', '{USER}', '{venue}', 'truss')"
        ),
        format!(
            "INSERT INTO venue_edges (child_id, uid, venue_id, parent_id, my_socket, their_socket)
             VALUES ('{child}', '{USER}', '{venue}', '{root}', 'base', 'top')"
        ),
        format!(
            "INSERT INTO venue_node_params (uid, venue_id, node_id, key, value)
             VALUES ('{USER}', '{venue}', '{child}', 'length', 3.0)"
        ),
        format!(
            "INSERT INTO venue_constraints
                 (uid, venue_id, node_id, my_socket, target_node, target_socket)
             VALUES ('{USER}', '{venue}', '{child}', 'base', '{root}', 'top')"
        ),
        format!(
            "INSERT INTO fixtures
                 (id, uid, venue_id, address, num_channels, manufacturer, model,
                  mode_name, fixture_path)
             VALUES ('{fixture}', '{USER}', '{venue}', 1, 8, 'Luma', 'Mover',
                     'Default', 'Luma/Mover.qxf')"
        ),
        format!(
            "INSERT INTO fixture_groups (id, uid, venue_id, name)
             VALUES ('{group}', '{USER}', '{venue}', 'front_wash')"
        ),
        format!(
            "INSERT INTO fixture_group_members (id, uid, venue_id, group_id, fixture_id)
             VALUES ('{group}:{fixture}', '{USER}', '{venue}', '{group}', '{fixture}')"
        ),
        format!(
            "INSERT INTO cues (id, uid, venue_id, name, pattern_id)
             VALUES ('{venue}-cue', '{USER}', '{venue}', 'Blinder', '{pattern}')"
        ),
        format!(
            "INSERT INTO midi_modifiers (id, uid, venue_id, name, input_json)
             VALUES ('{venue}-mod', '{USER}', '{venue}', 'shift', '{{}}')"
        ),
        format!(
            "INSERT INTO midi_bindings (id, uid, venue_id, trigger_json, action_json)
             VALUES ('{venue}-bind', '{USER}', '{venue}', '{{}}', '{{}}')"
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement.clone()))
            .execute(&a.sql)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    wait_for_upload(&a.sql).await;
    assert_eq!(rpc_join(&containers, OTHER, &code).await, venue);

    for (table, column, scope) in [
        ("venues", "id", venue.as_str()),
        ("venue_members", "venue_id", venue.as_str()),
        ("venue_nodes", "venue_id", venue.as_str()),
        ("venue_edges", "venue_id", venue.as_str()),
        ("venue_node_params", "venue_id", venue.as_str()),
        ("venue_constraints", "venue_id", venue.as_str()),
        ("fixtures", "venue_id", venue.as_str()),
        ("fixture_groups", "venue_id", venue.as_str()),
        ("fixture_group_members", "venue_id", venue.as_str()),
        ("cues", "venue_id", venue.as_str()),
        ("midi_modifiers", "venue_id", venue.as_str()),
        ("midi_bindings", "venue_id", venue.as_str()),
        ("scores", "venue_id", venue.as_str()),
        ("clips", "score_id", score.as_str()),
        ("score_definitions", "score_id", score.as_str()),
        ("tracks", "id", track.as_str()),
        ("track_beats", "track_id", track.as_str()),
        ("patterns", "id", pattern.as_str()),
        ("implementations", "pattern_id", pattern.as_str()),
    ] {
        settles(
            &b.sql,
            &count(table, column, scope),
            if table == "venue_nodes" { "2" } else { "1" },
            &format!("{table} never reached the member"),
        )
        .await;
    }

    a.close().await;
    b.close().await;
}

/// One PostgREST read as `user`.
async fn get(containers: &Containers, user: &str, path: &str) -> serde_json::Value {
    rest(containers, user, reqwest::Method::GET, path, None).await
}

/// One PostgREST update as `user`.
async fn patch(containers: &Containers, user: &str, path: &str, body: &serde_json::Value) {
    rest(containers, user, reqwest::Method::PATCH, path, Some(body)).await;
}

async fn rest(
    containers: &Containers,
    user: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<&serde_json::Value>,
) -> serde_json::Value {
    let (status, text) = try_rest(containers, user, method, path, body).await;
    assert!(status.is_success(), "{path} {status}: {text}");
    serde_json::from_str(text.trim()).unwrap_or(serde_json::Value::Null)
}

/// One PostgREST call whose refusal is the answer.
async fn try_rest(
    containers: &Containers,
    user: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<&serde_json::Value>,
) -> (reqwest::StatusCode, String) {
    let token = mint_jwt(&containers.secret, user);
    let url = format!("{}{path}", containers.postgrest.trim_end_matches('/'));
    let mut request = reqwest::Client::new()
        .request(method, &url)
        .header("apikey", &token)
        .header("Authorization", format!("Bearer {token}"));
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send().await.expect("postgrest");
    let status = response.status();
    (status, response.text().await.unwrap_or_default())
}

/// A fresh id, so two runs against a kept stack do not collide.
fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    )
}

/// Wait until this device has nothing left to upload.
async fn wait_for_upload(pool: &SqlitePool) {
    until(
        pool,
        "SELECT CAST(COUNT(*) AS TEXT) FROM ps_crud",
        "the upload queue never drained",
        |count| count == "0",
    )
    .await;
}

/// `public.join_venue(code)` over PostgREST, as the dispatch handler calls it.
async fn rpc_join(containers: &Containers, user: &str, code: &str) -> String {
    let token = mint_jwt(&containers.secret, user);
    let response = reqwest::Client::new()
        .post(format!(
            "{}/rpc/join_venue",
            containers.postgrest.trim_end_matches('/')
        ))
        .header("apikey", &token)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .expect("join_venue");
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert!(status.is_success(), "join_venue {status}: {body}");
    serde_json::from_str::<String>(body.trim()).expect("join_venue returns the venue id")
}
