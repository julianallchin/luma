//! Two app databases against one PowerSync instance.
//!
//! Raw-table statements, upload triggers and the PostgREST translation only
//! agree with each other through a real server, so these are the only tests
//! that can prove the client layer right. Every device here uploads through
//! the production [`Connector`], signed in with a minted token, so what is
//! tested is the translation that ships. They need the containers
//! `deploy/test-stack/run.py` brings up, and skip without them:
//!
//! ```text
//! python3 deploy/test-stack/run.py --test sync::two_device_tests
//! ```

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use luma_sync::powersync::{sdk::SyncOptions, Database, HttpClient};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::connector::Connector;
use super::schema::schema;
use super::service::{refused, DOWNLOAD_APPLIES};

/// The two accounts `deploy/test-stack/run.py` puts in `auth.users`
/// before it runs these tests. Fixed rather than minted here because the
/// server's foreign keys are real: a row whose `uid` is nobody is refused.
const USER: &str = "00000000-0000-0000-0000-0000000000aa";
const OTHER: &str = "00000000-0000-0000-0000-0000000000bb";
/// A third account that joins nothing. Every row it can see or write is a hole
/// in the policies.
const STRANGER: &str = "00000000-0000-0000-0000-0000000000cc";
const CONVERGE: Duration = Duration::from_secs(30);

/// The key id in `deploy/test-stack/service.yaml`'s inline JWKS.
const JWT_KID: &str = "luma-row-model-test";

struct Containers {
    powersync: String,
    postgrest: String,
    secret: String,
    /// Postgres itself, as its superuser: what an admin in the SQL editor is.
    postgres: String,
}

impl Containers {
    /// The environment `deploy/test-stack/run.py` exports, or nothing.
    fn from_env() -> Option<Self> {
        Some(Self {
            powersync: std::env::var("LUMA_TEST_POWERSYNC_URL").ok()?,
            postgrest: std::env::var("LUMA_TEST_POSTGREST_URL").ok()?,
            secret: std::env::var("LUMA_TEST_JWT_SECRET").ok()?,
            postgres: std::env::var("LUMA_TEST_PG_URI").ok()?,
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
    // `deploy/test-stack/service.yaml`. PowerSync refuses a token whose
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

/// One app database, opened exactly the way the app opens it: the real
/// migrations, the real upload triggers, the real PowerSync connection pair.
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

/// Point a device at the stack and start syncing, through the production
/// connector: a test mints its token rather than driving a Supabase sign-in,
/// and nothing else about the upload differs from the app's.
async fn connect(database: &Database, containers: &Containers, user: &str) {
    database
        .sync
        .connect(SyncOptions::new(Connector::with_token(
            database.sync.clone(),
            database.sql.clone(),
            mint_jwt(&containers.secret, user),
            containers.postgrest.clone(),
            containers.powersync.clone(),
        )))
        .await;
}

/// The writes this device made that the server refused, oldest first.
async fn not_saved(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT op || ' ' || table_name || ' ' || row_id || ' (' || status || '): '
                || COALESCE(body, '')
         FROM sync_rejections ORDER BY at, rowid",
    )
    .fetch_all(pool)
    .await
    .expect("refusals")
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
    lines.push(format!("not saved: {:?}", not_saved(pool).await));
    format!("device holds: {}", lines.join(" "))
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
/// with something in it. Written with the real upload triggers, so every row here
/// is also an upload.
async fn seed(pool: &SqlitePool, user: &str, venue: &str, score: &str, clips: &[(&str, f64)]) {
    sqlx::query(
        "INSERT INTO venues (id, uid, name, role)
         VALUES (?, ?, 'Test Room', 'owner')",
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
    for (label, start) in clips {
        clip(pool, user, score, &id(), label, *start, 8.0).await;
    }
}

/// One clip under `id`, which says nothing about it: a test names a clip by
/// its label.
async fn clip(
    pool: &SqlitePool,
    user: &str,
    score: &str,
    id: &str,
    label: &str,
    start: f64,
    duration: f64,
) {
    sqlx::query(
        "INSERT INTO clips (id, uid, score_id, label, name, graph_json, graph, start, duration,
             seed, selection_json, z_index, blend_mode)
         VALUES (?, ?, ?, ?, 'Strobe', '{\"version\":1,\"nodes\":{\"strobe1\":{\"kind\":\"strobe\"}}}', '', ?, ?, '1', '{\"all\":true}', 0,
                 'replace')",
    )
    .bind(id)
    .bind(user)
    .bind(score)
    .bind(label)
    .bind(start)
    .bind(duration)
    .execute(pool)
    .await
    .expect("clip");
}

/// The id of the clip a score labels `label`, on this device.
async fn clip_id(pool: &SqlitePool, score: &str, label: &str) -> String {
    sqlx::query_scalar("SELECT id FROM clips WHERE score_id = ? AND label = ?")
        .bind(score)
        .bind(label)
        .fetch_one(pool)
        .await
        .expect("the labelled clip")
}

/// The clips of one score, as `label=duration` pairs. What every convergence
/// assertion below compares.
///
/// Scoped to one score because the containers outlive one test run: two runs
/// against a kept stack share a database, and an unscoped count would be a
/// test of how many times it had been run.
fn clip_shape(score: &str) -> String {
    format!(
        "SELECT group_concat(label || '=' || CAST(duration AS TEXT), ',') FROM (
             SELECT label, duration FROM clips WHERE score_id = '{score}' ORDER BY label)"
    )
}

/// A venue's stage and patch: a root, a node under it with a parameter and a
/// constraint, a fixture, and a group the fixture is in. One row of every
/// shape that hangs off another.
struct Rig {
    child: String,
    fixture: String,
    group: String,
}

async fn rig(pool: &SqlitePool, user: &str, venue: &str) -> Rig {
    let rig = Rig {
        child: id(),
        fixture: id(),
        group: id(),
    };
    let Rig {
        child,
        fixture,
        group,
    } = &rig;
    let root = id();
    for statement in [
        format!(
            "INSERT INTO venue_nodes
                 (id, uid, venue_id, kind, parent_id, my_socket, their_socket, roll)
             VALUES ('{root}', '{user}', '{venue}', 'venue', NULL, NULL, NULL, NULL),
                    ('{child}', '{user}', '{venue}', 'run', '{root}', 'base', 'top', 0.0)"
        ),
        format!(
            "INSERT INTO venue_node_params (id, uid, venue_id, node_id, key, value)
             VALUES ('{}', '{user}', '{venue}', '{child}', 'length', 3.0)",
            id()
        ),
        format!(
            "INSERT INTO venue_constraints
                 (id, uid, venue_id, node_id, my_socket, target_node, target_socket)
             VALUES ('{}', '{user}', '{venue}', '{child}', 'base', '{root}', 'top')",
            id()
        ),
        format!(
            "INSERT INTO fixtures
                 (id, uid, venue_id, address, num_channels, manufacturer, model,
                  mode_name, fixture_path)
             VALUES ('{fixture}', '{user}', '{venue}', 1, 8, 'Luma', 'Mover',
                     'Default', 'Luma/Mover.qxf')"
        ),
        format!(
            "INSERT INTO fixture_groups (id, uid, venue_id, name)
             VALUES ('{group}', '{user}', '{venue}', 'front_wash')"
        ),
        format!(
            "INSERT INTO fixture_group_members (id, uid, venue_id, group_id, fixture_id)
             VALUES ('{}', '{user}', '{venue}', '{group}', '{fixture}')",
            id()
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement.clone()))
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    rig
}

/// Postgres as its superuser, to read what the server keeps and no device
/// is sent.
async fn admin(containers: &Containers) -> sqlx::PgPool {
    sqlx::PgPool::connect(&containers.postgres)
        .await
        .expect("postgres")
}

/// Whether the server's row is flagged, and by whom. Panics if the server
/// does not have the row: a source row is never removed.
async fn flag(admin: &sqlx::PgPool, table: &str, id: &str) -> (bool, Option<String>) {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select deleted_at is not null, deleted_by::text from public.{table} where id = $1"
    )))
    .bind(id)
    .fetch_one(admin)
    .await
    .unwrap_or_else(|error| panic!("the server has no {table} {id}: {error}"))
}

/// How many rows of `table` belong to `scope`.
fn count(table: &str, column: &str, scope: &str) -> String {
    format!("SELECT CAST(COUNT(*) AS TEXT) FROM {table} WHERE {column} = '{scope}'")
}

/// Two devices, one account: a score written on A appears on B, each edits,
/// and both land on the same rows.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
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
    sqlx::query("UPDATE clips SET duration = 16.0 WHERE score_id = ? AND label = 'one'")
        .bind(&score)
        .execute(&b.sql)
        .await
        .expect("edit");
    sqlx::query("DELETE FROM clips WHERE score_id = ? AND label = 'two'")
        .bind(&score)
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
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
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
    sqlx::query("UPDATE clips SET duration = 32.0 WHERE score_id = ? AND label = 'one'")
        .bind(&score)
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
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
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
    sqlx::query("UPDATE clips SET duration = 12.0 WHERE score_id = ? AND label = 'one'")
        .bind(&score)
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

    // Revocation is a delete of the membership row, which the server keeps
    // flagged, and it reaches B as a download that empties every bucket the
    // membership put them in.
    let membership: String = sqlx::query_scalar("SELECT id FROM venue_members WHERE venue_id = ?")
        .bind(&venue)
        .fetch_one(&a.sql)
        .await
        .expect("the membership");
    sqlx::query("DELETE FROM venue_members WHERE venue_id = ?")
        .bind(&venue)
        .execute(&a.sql)
        .await
        .expect("revoke");
    wait_for_upload(&a.sql).await;
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

    // The flagged membership is still B's row, and clearing a flag is how a
    // row comes back: B must not be able to walk back in that way.
    let (status, body) = try_rest(
        &containers,
        OTHER,
        reqwest::Method::PATCH,
        &format!("/venue_members?id=eq.{membership}"),
        Some(&serde_json::json!({ "deleted_at": null })),
    )
    .await;
    assert!(
        status.is_client_error(),
        "a removed member restored their own membership: {status} {body}"
    );
    assert_eq!(
        get(
            &containers,
            OTHER,
            &format!("/venues?id=eq.{venue}&select=id")
        )
        .await,
        serde_json::json!([]),
        "a removed member can read the venue again"
    );

    a.close().await;
    b.close().await;
}

/// The app's own sharing commands, end to end: the owner mints a code through
/// the service, a second account redeems it through the service and gets the
/// venue back once it has downloaded, a revoked code admits nobody, and the
/// owner removing a member takes the venue off their device.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn the_sharing_service_admits_with_a_code_and_revoking_it_admits_nobody() {
    use crate::services::venue_sharing::{self, Refusal, ShareCodeServer};

    /// The production redemption with a minted token in place of the state
    /// database's session.
    struct Minted(crate::sync::supabase::Postgrest, String);

    #[async_trait::async_trait]
    impl ShareCodeServer for Minted {
        async fn redeem(&self, code: &str) -> Result<String, Refusal> {
            venue_sharing::redeem_with_token(&self.0, &self.1, code).await
        }
    }

    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let server = |user: &str| {
        let token = mint_jwt(&containers.secret, user);
        Minted(
            crate::sync::supabase::Postgrest::new(containers.postgrest.clone(), token.clone()),
            token,
        )
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, OTHER).await;

    let venue = unique("venue");
    let score = unique("score");
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0)]).await;
    // A join waits for the venue's root, as every venue written today has.
    sqlx::query("INSERT INTO venue_nodes (id, uid, venue_id, kind) VALUES (?, ?, ?, 'venue')")
        .bind(unique("root"))
        .bind(USER)
        .bind(&venue)
        .execute(&a.sql)
        .await
        .expect("root");
    let code = venue_sharing::rotate_share_code(&a.sql, &venue)
        .await
        .expect("mint a code");
    wait_for_upload(&a.sql).await;

    let joined_id = venue_sharing::join(&b.sql, &server(OTHER), &code)
        .await
        .expect("join with the code");
    assert_eq!(joined_id, venue);
    let deadline = std::time::Instant::now() + CONVERGE;
    let joined = loop {
        if let Some(joined) = venue_sharing::arrived(&b.sql, &joined_id).await {
            break joined;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the joined venue never downloaded"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(joined.is_member());
    settles(
        &b.sql,
        &count("clips", "score_id", &score),
        "1",
        "B joined but the venue's score never arrived",
    )
    .await;
    settles(
        &a.sql,
        &count("venue_members", "venue_id", &venue),
        "1",
        "the owner never saw the membership",
    )
    .await;
    let members = venue_sharing::members(&a.sql, &venue)
        .await
        .expect("the owner lists members");
    assert_eq!(
        members
            .iter()
            .map(|member| member.uid.as_str())
            .collect::<Vec<_>>(),
        [USER, OTHER]
    );

    venue_sharing::revoke_share_code(&a.sql, &venue)
        .await
        .expect("revoke");
    wait_for_upload(&a.sql).await;
    assert_eq!(
        server(STRANGER).redeem(&code).await,
        Err(Refusal::UnknownCode),
        "a revoked code still admitted someone"
    );
    assert_eq!(
        get(
            &containers,
            STRANGER,
            &format!("/venues?id=eq.{venue}&select=id")
        )
        .await,
        serde_json::json!([])
    );

    venue_sharing::remove_member(&a.sql, &venue, OTHER)
        .await
        .expect("remove the member");
    settles(
        &b.sql,
        &count("venues", "id", &venue),
        "0",
        "B kept the venue after the owner removed them",
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
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
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
            name: "Strobe".into(),
            label: None,
            graph: serde_json::from_value(serde_json::json!(
                {"version": 3, "nodes": {"strobe1": {"kind": "strobe"}}}
            ))
            .expect("a strobe graph"),
            start: 8.0,
            duration: 4.0,
            seed: 7,
            selection: luma_patterns::Selection::all(),
            z_index: 0,
            blend_mode: luma_patterns::BlendMode::Replace,
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
        &format!(
            "SELECT group_concat(CAST(duration AS TEXT), ',') FROM (
                 SELECT duration FROM clips WHERE score_id = '{score}' ORDER BY start)"
        ),
        "8.0,4.0",
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
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
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
    let principal = crate::database::local::auth::principal_key(USER);
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

/// What the record keeps beside the row: the build that wrote a clip and the
/// venue it was written against, a copy's source, and a person's verdict,
/// which follows its author to their other device and is kept when taken
/// back.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn the_record_keeps_the_build_the_venue_version_and_a_verdict() {
    use crate::database::local::feedback::{self, Target, Verdict};

    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, USER).await;
    let admin = admin(&containers).await;

    let (venue, score) = (unique("venue"), unique("score"));
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0)]).await;
    wait_for_upload(&a.sql).await;
    let one = clip_id(&a.sql, &score, "one").await;

    // The venue changes, then the score is written again: the second clip
    // names a later venue than the first.
    sqlx::query("UPDATE venues SET name = 'Main Room' WHERE id = ?")
        .bind(&venue)
        .execute(&a.sql)
        .await
        .expect("rename");
    wait_for_upload(&a.sql).await;
    clip(&a.sql, USER, &score, &id(), "two", 8.0, 8.0).await;
    wait_for_upload(&a.sql).await;
    let entries: Vec<(String, Option<String>, Option<i64>)> = sqlx::query_as(
        "select after ->> 'label', client_build, venue_seq from public.history
         where table_name = 'clips' and scope_id = $1 order by seq",
    )
    .bind(&score)
    .fetch_all(&admin)
    .await
    .expect("history");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.0.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    for (label, build, _) in &entries {
        assert_eq!(build.as_deref(), Some(crate::build().as_str()), "{label}");
    }
    let newest_venue_entry: i64 =
        sqlx::query_scalar("select max(seq) from public.history where scope_id = $1")
            .bind(&venue)
            .fetch_one(&admin)
            .await
            .expect("venue history");
    assert!(entries[0].2.expect("the first clip's venue") < newest_venue_entry);
    assert_eq!(entries[1].2, Some(newest_venue_entry));

    // A copy names what it was copied from, on the server too.
    let copy = crate::services::catalog::duplicate_score(&a.sql, &score, "Copy")
        .await
        .expect("copy");
    wait_for_upload(&a.sql).await;
    let source: Option<String> =
        sqlx::query_scalar("select copied_from_score_id from public.scores where id = $1")
            .bind(&copy.id)
            .fetch_one(&admin)
            .await
            .expect("the copy");
    assert_eq!(source.as_deref(), Some(score.as_str()));

    // A verdict on the clip reaches the other device, changes there, and is
    // flagged, not removed, when it is taken back.
    let target = Target::Clip {
        clip_id: one.clone(),
    };
    feedback::set(&a.sql, &target, Some(Verdict::Bad), Some("too busy"), USER)
        .await
        .expect("verdict");
    let held = format!("SELECT group_concat(rating) FROM feedback WHERE clip_id = '{one}'");
    settles(
        &b.sql,
        &held,
        "-1",
        "the verdict did not reach the other device",
    )
    .await;
    feedback::set(&b.sql, &target, Some(Verdict::Good), None, USER)
        .await
        .expect("a changed mind");
    settles(&a.sql, &held, "1", "the changed verdict did not come back").await;
    feedback::set(&a.sql, &target, None, None, USER)
        .await
        .expect("taken back");
    settles(
        &b.sql,
        &count("feedback", "clip_id", &one),
        "0",
        "the verdict taken back stayed on the other device",
    )
    .await;
    let (rows, flagged, in_scope): (i64, i64, i64) = sqlx::query_as(
        "select count(*), count(deleted_at),
                (select count(*) from public.history
                 where table_name = 'feedback' and scope_id = $2)
         from public.feedback where clip_id = $1",
    )
    .bind(&one)
    .bind(&score)
    .fetch_one(&admin)
    .await
    .expect("the server's verdict");
    assert_eq!((rows, flagged), (1, 1), "one row, kept and flagged");
    assert!(
        in_scope >= 3,
        "made, changed and taken back are all in the score's history"
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
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
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
            "selection_json": "{\"all\":true}",
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
/// This is what evaluates each query in `deploy/powersync/sync-config.yaml` against real
/// data: a rule that names a column the table does not have, or a subquery the
/// service refuses, fails the whole stream, and the only symptom is rows that
/// never arrive.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
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
    for statement in [
        format!("UPDATE venues SET share_code = '{code}' WHERE id = '{venue}'"),
        format!(
            "INSERT INTO track_beats (id, track_id, uid, beats_json, downbeats_json)
             VALUES ('{}', '{track}', '{USER}', '[]', '[]')",
            id()
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement.clone()))
            .execute(&a.sql)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    rig(&a.sql, USER, &venue).await;
    wait_for_upload(&a.sql).await;
    assert_eq!(rpc_join(&containers, OTHER, &code).await, venue);

    for (table, column, scope) in [
        ("venues", "id", venue.as_str()),
        ("venue_members", "venue_id", venue.as_str()),
        ("venue_nodes", "venue_id", venue.as_str()),
        ("venue_node_params", "venue_id", venue.as_str()),
        ("venue_constraints", "venue_id", venue.as_str()),
        ("fixtures", "venue_id", venue.as_str()),
        ("fixture_groups", "venue_id", venue.as_str()),
        ("fixture_group_members", "venue_id", venue.as_str()),
        ("scores", "venue_id", venue.as_str()),
        ("clips", "score_id", score.as_str()),
        ("tracks", "id", track.as_str()),
        ("track_beats", "track_id", track.as_str()),
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

/// A member edits the venue row itself — its environment and haze — and a
/// fixture under it, and the owner sees both. The venue's owner and share
/// code stay the owner's to change, and the venue the owner's to delete.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn a_member_edits_the_venue_but_not_its_door() {
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
    wait_for_upload(&a.sql).await;
    assert_eq!(rpc_join(&containers, OTHER, &code).await, venue);
    settles(
        &b.sql,
        &count("venues", "id", &venue),
        "1",
        "the venue never reached the member",
    )
    .await;

    // The member's device uploads the edits; waiting for the upload fails the
    // test on any refusal.
    let environment = r#"{"mode":"indoor","houseLevel":0.25}"#;
    let haze = r#"{"enabled":false,"density":0.1}"#;
    sqlx::query("UPDATE venues SET environment = ?, haze = ? WHERE id = ?")
        .bind(environment)
        .bind(haze)
        .bind(&venue)
        .execute(&b.sql)
        .await
        .expect("member venue edit");
    let fixture = unique("fixture");
    sqlx::query(
        "INSERT INTO fixtures
             (id, uid, venue_id, address, num_channels, manufacturer, model,
              mode_name, fixture_path)
         VALUES (?, ?, ?, 1, 8, 'Luma', 'Mover', 'Default', 'Luma/Mover.qxf')",
    )
    .bind(&fixture)
    .bind(OTHER)
    .bind(&venue)
    .execute(&b.sql)
    .await
    .expect("member fixture");
    wait_for_upload(&b.sql).await;
    settles(
        &a.sql,
        &format!("SELECT environment || haze FROM venues WHERE id = '{venue}'"),
        &format!("{environment}{haze}"),
        "the owner never saw the member's environment and haze",
    )
    .await;
    settles(
        &a.sql,
        &count("fixtures", "id", &fixture),
        "1",
        "the owner never saw the member's fixture",
    )
    .await;

    for (column, value) in [
        ("share_code", serde_json::json!(unique("taken"))),
        ("uid", serde_json::json!(OTHER)),
    ] {
        let (status, body) = try_rest(
            &containers,
            OTHER,
            reqwest::Method::PATCH,
            &format!("/venues?id=eq.{venue}"),
            Some(&serde_json::json!({ column: value })),
        )
        .await;
        assert!(
            status.is_client_error(),
            "a member changed the venue's {column}: {status} {body}"
        );
    }
    // Deleting the venue is the owner's too, in either spelling: the flag a
    // delete uploads as, and the DELETE no account is granted any more.
    for (method, body) in [
        (
            reqwest::Method::PATCH,
            Some(serde_json::json!({ "deleted_at": "2026-11-12T00:00:00.000Z" })),
        ),
        (reqwest::Method::DELETE, None),
    ] {
        let (status, text) = try_rest(
            &containers,
            OTHER,
            method.clone(),
            &format!("/venues?id=eq.{venue}"),
            body.as_ref(),
        )
        .await;
        assert!(
            status.is_client_error(),
            "a member's {method} of the venue was not refused: {status} {text}"
        );
    }
    assert_eq!(
        get(
            &containers,
            USER,
            &format!("/venues?id=eq.{venue}&select=uid,share_code,deleted_at"),
        )
        .await,
        serde_json::json!([{ "uid": USER, "share_code": code, "deleted_at": null }]),
        "a member moved the venue's door"
    );

    a.close().await;
    b.close().await;
}

/// The owner brings back a membership they removed, by inserting the row
/// again as undo does. One the member ended themselves stays ended, and the
/// owner makes nobody a member who did not join.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn an_owner_restores_only_a_membership_they_removed() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let venue = unique("venue");
    seed(&a.sql, USER, &venue, &unique("score"), &[]).await;
    let code = unique("door");
    sqlx::query("UPDATE venues SET share_code = ? WHERE id = ?")
        .bind(&code)
        .bind(&venue)
        .execute(&a.sql)
        .await
        .expect("share code");
    wait_for_upload(&a.sql).await;
    rpc_join(&containers, OTHER, &code).await;
    let membership = get(
        &containers,
        USER,
        &format!("/venue_members?venue_id=eq.{venue}&uid=eq.{OTHER}"),
    )
    .await[0]
        .clone();
    let member = membership["id"].as_str().expect("the membership's id");
    let flag = serde_json::json!({ "deleted_at": "2026-11-12T00:00:00.000Z" });
    let row = format!("/venue_members?id=eq.{member}");
    let insert = |user: &'static str, row: serde_json::Value| {
        let containers = &containers;
        async move {
            let token = mint_jwt(&containers.secret, user);
            let response = reqwest::Client::new()
                .post(format!(
                    "{}/venue_members",
                    containers.postgrest.trim_end_matches('/')
                ))
                .header("apikey", &token)
                .header("Authorization", format!("Bearer {token}"))
                .header(super::supabase::FORMAT_HEADER, FORMAT)
                .header("Prefer", "resolution=merge-duplicates,return=minimal")
                .json(&row)
                .send()
                .await
                .expect("postgrest");
            response.status()
        }
    };
    let live = || async {
        get(&containers, USER, &format!("{row}&select=deleted_at")).await[0]["deleted_at"].is_null()
    };

    // The owner removes the member, then undoes it.
    rest(&containers, USER, reqwest::Method::PATCH, &row, Some(&flag)).await;
    assert!(
        !live().await,
        "the owner's removal did not flag the membership"
    );
    let status = insert(USER, membership.clone()).await;
    assert!(
        status.is_success(),
        "the owner's undo was refused: {status}"
    );
    assert!(live().await, "the owner's undo left the membership removed");

    // The member leaves. Neither the owner nor the member inserts it back.
    rest(
        &containers,
        OTHER,
        reqwest::Method::PATCH,
        &row,
        Some(&flag),
    )
    .await;
    for user in [USER, OTHER] {
        let status = insert(user, membership.clone()).await;
        assert!(
            status.is_client_error(),
            "{user} restored a membership the member ended: {status}"
        );
    }
    assert!(!live().await, "a membership its member ended is live again");

    // And the owner adds nobody who never joined.
    let mut stranger = membership.clone();
    stranger["id"] = serde_json::json!(id());
    let status = insert(USER, stranger).await;
    assert!(
        status.is_client_error(),
        "the owner made a member without a code: {status}"
    );

    a.close().await;
}

/// A write the server's policies refuse is not lost in silence. A member may
/// read the owner's song but not retitle it: the update is filtered to no row
/// and PostgREST answers 204. The production connector counts the rows its
/// update changed, records the refusal once, takes it off the queue, and the
/// next download puts the owner's title back. The status bar says so.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn a_refused_write_is_one_rejection_and_reads_not_saved() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let venue = unique("venue");
    let score = unique("score");
    let code = unique("code");
    let track = format!("{venue}-track");
    seed(&a.sql, USER, &venue, &score, &[]).await;
    sqlx::query("UPDATE venues SET share_code = ? WHERE id = ?")
        .bind(&code)
        .bind(&venue)
        .execute(&a.sql)
        .await
        .expect("share code");
    wait_for_upload(&a.sql).await;
    assert_eq!(rpc_join(&containers, OTHER, &code).await, venue);

    // The member's device, syncing the way the app does: the production
    // connector under the production status service.
    let b = offline_device(&directory.path().join("b"), OTHER).await;
    let sync = b.sync.clone();
    let state = SqlitePool::connect("sqlite::memory:").await.expect("state");
    let service = super::service::Service::start(b, state, crate::events::Events::discard())
        .expect("service");
    sync.connect(SyncOptions::new(Connector::with_token(
        sync.clone(),
        service.pool().clone(),
        mint_jwt(&containers.secret, OTHER),
        containers.postgrest.clone(),
        containers.powersync.clone(),
    )))
    .await;
    let pool = service.pool().clone();
    settles(
        &pool,
        &count("tracks", "id", &track),
        "1",
        "the owner's song never reached the member",
    )
    .await;

    sqlx::query("UPDATE tracks SET title = 'Retitled' WHERE id = ?")
        .bind(&track)
        .execute(&pool)
        .await
        .expect("member retitles the owner's song");
    drained(&pool).await;
    settles(
        &pool,
        &format!("SELECT title FROM tracks WHERE id = '{track}'"),
        "Aurora",
        "the server's title never came back",
    )
    .await;

    let status = service.status().await.expect("status");
    assert_eq!(
        status
            .refused
            .iter()
            .map(|refusal| (
                refusal.table.as_str(),
                refusal.row_id.as_str(),
                refusal.op.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![("tracks", track.as_str(), "PATCH")],
        "the refusal is recorded once"
    );
    assert_eq!(status.summary().to_string(), "1 not saved");

    service.dismiss_refusals().await.expect("dismiss");
    assert!(service.status().await.expect("status").refused.is_empty());

    service.stop();
    a.close().await;
}

/// A build behind the server's data format is turned away at both ends: its
/// writes are refused with a 426 on every synced table, and it does not open
/// a sync stream, so nothing it would misread reaches it. The status says an
/// update is required. So is a build from before the header, which reads
/// format 1. A build at the server's format, or ahead of it, is served as
/// ever.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn a_build_behind_the_servers_format_neither_writes_nor_reads() {
    let Some(containers) = Containers::from_env() else {
        eprintln!(
            "skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET / _PG_URI unset"
        );
        return;
    };
    let admin = admin(&containers).await;
    let server: i32 = sqlx::query_scalar("select public.data_format()")
        .fetch_one(&admin)
        .await
        .expect("the server's format");
    assert_eq!(
        i64::from(server),
        crate::database::local::database::PROJECTION_VERSION,
        "this build reads the format the migrations leave the server at"
    );
    let guarded: Vec<String> = sqlx::query_scalar(
        "select event_object_table::text from information_schema.triggers
         where trigger_schema = 'public' and trigger_name like '%\\_client\\_format'",
    )
    .fetch_all(&admin)
    .await
    .expect("triggers");
    for table in super::schema::SYNCED_TABLES {
        assert!(
            guarded.iter().any(|name| name == table.name),
            "{} takes writes from a build of any format",
            table.name
        );
    }

    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let venue = unique("venue");
    let score = unique("score");
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0)]).await;
    wait_for_upload(&a.sql).await;

    // The write, as builds of different formats send it.
    let rename = |format: Option<i64>| {
        let containers = &containers;
        let venue = &venue;
        async move {
            let token = mint_jwt(&containers.secret, USER);
            let mut request = reqwest::Client::new()
                .patch(format!(
                    "{}/venues?id=eq.{venue}",
                    containers.postgrest.trim_end_matches('/')
                ))
                .header("apikey", &token)
                .header("Authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({ "name": "Renamed" }));
            if let Some(format) = format {
                request = request.header(super::supabase::FORMAT_HEADER, format);
            }
            request.send().await.expect("postgrest").status().as_u16()
        }
    };
    let server = i64::from(server);
    assert_eq!(rename(Some(server - 1)).await, 426, "a build behind");
    let name: String = sqlx::query_scalar("select name from public.venues where id = $1")
        .bind(&venue)
        .fetch_one(&admin)
        .await
        .expect("venue");
    assert_eq!(name, "Test Room", "the refused write changed the row");
    assert!(rename(Some(server)).await < 300, "a build at the format");
    assert!(rename(Some(server + 1)).await < 300, "a build ahead");
    assert_eq!(
        rename(None).await,
        426,
        "a build from before the header reads format 1"
    );

    // The same account on a build one format behind, syncing as the app does.
    let b = offline_device(&directory.path().join("b"), USER).await;
    let sync = b.sync.clone();
    let state = SqlitePool::connect("sqlite::memory:").await.expect("state");
    let service = super::service::Service::start(b, state, crate::events::Events::discard())
        .expect("service");
    sync.connect(SyncOptions::new(
        Connector::with_token(
            sync.clone(),
            service.pool().clone(),
            mint_jwt(&containers.secret, USER),
            containers.postgrest.clone(),
            containers.powersync.clone(),
        )
        .reading(server - 1, service.outdated()),
    ))
    .await;
    let deadline = Instant::now() + CONVERGE;
    while !service.status().await.expect("status").update_required {
        assert!(
            Instant::now() < deadline,
            "the build behind never learned it was"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let status = service.status().await.expect("status");
    assert_eq!(status.summary().to_string(), "Update required");
    let held: String = sqlx::query_scalar(sqlx::AssertSqlSafe(count("clips", "score_id", &score)))
        .fetch_one(service.pool())
        .await
        .expect("count");
    assert_eq!(held, "0", "the build behind applied rows it cannot read");

    service.stop();
    a.close().await;
}

/// A member's fixture outlives the member: nothing cascades from `auth.users`
/// into a shared venue, so deleting the member's auth user is refused while
/// they authored anything there, and the fixture stays in the owner's venue.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn a_members_fixtures_survive_deleting_the_member() {
    let Some(containers) = Containers::from_env() else {
        eprintln!(
            "skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET / _PG_URI unset"
        );
        return;
    };
    let admin = admin(&containers).await;
    // An account of its own: this test deletes it.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let member = format!("00000000-0000-0000-0000-{:012x}", nanos & 0xffff_ffff_ffff);
    sqlx::query("INSERT INTO auth.users (id) VALUES ($1::uuid)")
        .bind(&member)
        .execute(&admin)
        .await
        .expect("the member's auth user");

    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, &member).await;

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
    wait_for_upload(&a.sql).await;
    assert_eq!(rpc_join(&containers, &member, &code).await, venue);
    settles(
        &b.sql,
        &count("venues", "id", &venue),
        "1",
        "the venue never reached the member",
    )
    .await;

    let fixture = unique("fixture");
    sqlx::query(
        "INSERT INTO fixtures
             (id, uid, venue_id, address, num_channels, manufacturer, model,
              mode_name, fixture_path)
         VALUES (?, ?, ?, 1, 8, 'Luma', 'Mover', 'Default', 'Luma/Mover.qxf')",
    )
    .bind(&fixture)
    .bind(&member)
    .bind(&venue)
    .execute(&b.sql)
    .await
    .expect("member fixture");
    wait_for_upload(&b.sql).await;
    settles(
        &a.sql,
        &count("fixtures", "id", &fixture),
        "1",
        "the owner never saw the member's fixture",
    )
    .await;

    // An admin deleting the auth user outright is refused: the fixture names
    // its author, and nothing cascades from auth.users into a shared venue.
    let refused = sqlx::query("DELETE FROM auth.users WHERE id = $1::uuid")
        .bind(&member)
        .execute(&admin)
        .await
        .expect_err("deleting an author's auth user must be refused");
    assert!(
        refused
            .as_database_error()
            .is_some_and(|error| error.code().as_deref() == Some("23503")),
        "refused for the wrong reason: {refused}"
    );

    // The fixture, and the membership, stay where they were.
    assert_eq!(
        get(
            &containers,
            USER,
            &format!("/fixtures?id=eq.{fixture}&select=uid"),
        )
        .await,
        serde_json::json!([{ "uid": member }]),
        "the member's fixture did not survive deleting their auth user"
    );
    for (table, column, scope) in [
        ("fixtures", "id", fixture.as_str()),
        ("venue_members", "venue_id", venue.as_str()),
    ] {
        settles(
            &a.sql,
            &count(table, column, scope),
            "1",
            &format!("{table} left the owner's device with the member's auth user"),
        )
        .await;
    }

    a.close().await;
    b.close().await;
}

/// A row deleted on one device leaves the other, and the server keeps it:
/// flagged, and stamped with who deleted it. The delete is not a removal
/// anywhere but on devices.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn a_deleted_row_leaves_the_other_device_and_stays_on_the_server_flagged() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let admin = admin(&containers).await;
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
    let kept = clip_id(&a.sql, &score, "one").await;
    let deleted = clip_id(&a.sql, &score, "two").await;

    sqlx::query("DELETE FROM clips WHERE id = ?")
        .bind(&deleted)
        .execute(&a.sql)
        .await
        .expect("delete");
    wait_for_upload(&a.sql).await;
    for (name, pool) in [("A", &a.sql), ("B", &b.sql)] {
        settles(
            pool,
            &clip_shape(&score),
            "one=8.0",
            &format!("{name} still holds the deleted clip"),
        )
        .await;
    }

    assert_eq!(
        flag(&admin, "clips", &deleted).await,
        (true, Some(USER.to_owned())),
        "the server keeps the deleted clip, flagged by who deleted it"
    );
    assert_eq!(flag(&admin, "clips", &kept).await, (false, None));

    a.close().await;
    b.close().await;
}

/// Deleting a score takes its clips with it everywhere: both devices lose
/// them, and the server flags the clips along with the score rather than
/// leaving live children under a flagged parent. The device's own cascade
/// uploads a delete per clip as well, and none of those is a refusal.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn deleting_a_score_takes_its_clips_off_both_devices() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let admin = admin(&containers).await;
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
    let clips = [
        clip_id(&a.sql, &score, "one").await,
        clip_id(&a.sql, &score, "two").await,
    ];

    sqlx::query("DELETE FROM scores WHERE id = ?")
        .bind(&score)
        .execute(&a.sql)
        .await
        .expect("delete the score");
    wait_for_upload(&a.sql).await;
    for (name, pool) in [("A", &a.sql), ("B", &b.sql)] {
        for (table, column) in [("scores", "id"), ("clips", "score_id")] {
            settles(
                pool,
                &count(table, column, &score),
                "0",
                &format!("{name} kept the deleted score's {table}"),
            )
            .await;
        }
    }

    assert!(
        flag(&admin, "scores", &score).await.0,
        "the score is flagged"
    );
    for clip in &clips {
        assert_eq!(
            flag(&admin, "clips", clip).await,
            (true, Some(USER.to_owned())),
            "the score's clip is flagged with it"
        );
    }
    // The venue the score was in is untouched.
    assert_eq!(flag(&admin, "venues", &venue).await, (false, None));

    a.close().await;
    b.close().await;
}

/// Undo is inserting the row again under its id: the upsert clears the
/// server's flag, and the row comes back on both devices as the row it was.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn reinserting_a_deleted_row_under_its_id_brings_it_back() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let admin = admin(&containers).await;
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
        "B did not receive the clip",
    )
    .await;
    let clip_one = clip_id(&a.sql, &score, "one").await;

    sqlx::query("DELETE FROM clips WHERE id = ?")
        .bind(&clip_one)
        .execute(&a.sql)
        .await
        .expect("delete");
    wait_for_upload(&a.sql).await;
    settles(
        &b.sql,
        &count("clips", "score_id", &score),
        "0",
        "B kept the deleted clip",
    )
    .await;
    assert!(flag(&admin, "clips", &clip_one).await.0);

    // The same id, and a different length so the row that returns is known
    // to be the one re-inserted.
    clip(&a.sql, USER, &score, &clip_one, "one", 0.0, 4.0).await;
    wait_for_upload(&a.sql).await;
    for (name, pool) in [("A", &a.sql), ("B", &b.sql)] {
        settles(
            pool,
            &format!(
                "SELECT group_concat(id || '=' || CAST(duration AS TEXT), ',')
                 FROM clips WHERE score_id = '{score}'"
            ),
            &format!("{clip_one}=4.0"),
            &format!("{name} did not get the clip back under its id"),
        )
        .await;
    }
    assert_eq!(
        flag(&admin, "clips", &clip_one).await,
        (false, None),
        "the re-insert cleared the flag and who set it"
    );

    a.close().await;
    b.close().await;
}

/// An edit that reaches the server after its row was deleted changes
/// nothing. B edits a clip offline while A deletes it; when B reconnects its
/// edit is one refused write, listed as not saved rather than lost in
/// silence, the clip leaves B, and the server's flagged row is as A left it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn an_offline_edit_to_a_row_deleted_meanwhile_is_not_saved() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let admin = admin(&containers).await;
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
        "B did not receive the clip",
    )
    .await;
    let clip_one = clip_id(&a.sql, &score, "one").await;

    b.sync.disconnect().await;
    sqlx::query("UPDATE clips SET duration = 32.0 WHERE id = ?")
        .bind(&clip_one)
        .execute(&b.sql)
        .await
        .expect("offline edit");
    sqlx::query("DELETE FROM clips WHERE id = ?")
        .bind(&clip_one)
        .execute(&a.sql)
        .await
        .expect("delete");
    wait_for_upload(&a.sql).await;
    assert!(flag(&admin, "clips", &clip_one).await.0);

    connect(&b, &containers, USER).await;
    drained(&b.sql).await;
    settles(
        &b.sql,
        &count("clips", "score_id", &score),
        "0",
        "B kept the clip A deleted",
    )
    .await;
    let refused: Vec<(String, String, String)> =
        sqlx::query_as("SELECT table_name, row_id, op FROM sync_rejections")
            .fetch_all(&b.sql)
            .await
            .expect("refusals");
    assert_eq!(
        refused,
        [("clips".to_owned(), clip_one.clone(), "PATCH".to_owned())],
        "B's edit is one refused write"
    );
    let (flagged, duration): (bool, f64) =
        sqlx::query_as("select deleted_at is not null, duration from public.clips where id = $1")
            .bind(&clip_one)
            .fetch_one(&admin)
            .await
            .expect("the server's clip");
    assert!(flagged, "B's edit brought the clip back");
    assert!(
        (duration - 8.0).abs() < f64::EPSILON,
        "B's edit reached the flagged row: {duration}"
    );

    a.close().await;
    b.close().await;
}

/// A row written offline under a parent another device deleted meanwhile is
/// refused, as the foreign key refused it when a delete removed the parent.
/// Accepted, it would be a live row under a parent no device holds.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn an_offline_child_of_a_parent_deleted_meanwhile_is_not_saved() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let admin = admin(&containers).await;
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, USER).await;

    let venue = unique("venue");
    let score = unique("score");
    seed(&a.sql, USER, &venue, &score, &[]).await;
    let rig = rig(&a.sql, USER, &venue).await;
    settles(
        &b.sql,
        &count("venue_nodes", "venue_id", &venue),
        "2",
        "B did not receive the stage",
    )
    .await;

    b.sync.disconnect().await;
    let param = id();
    sqlx::query(
        "INSERT INTO venue_node_params (id, uid, venue_id, node_id, key, value)
         VALUES (?, ?, ?, ?, 'width', 2.0)",
    )
    .bind(&param)
    .bind(USER)
    .bind(&venue)
    .bind(&rig.child)
    .execute(&b.sql)
    .await
    .expect("offline parameter");
    sqlx::query("DELETE FROM venue_nodes WHERE id = ?")
        .bind(&rig.child)
        .execute(&a.sql)
        .await
        .expect("delete the node");
    wait_for_upload(&a.sql).await;
    assert!(flag(&admin, "venue_nodes", &rig.child).await.0);

    connect(&b, &containers, USER).await;
    drained(&b.sql).await;
    for (table, rows) in [("venue_nodes", "1"), ("venue_node_params", "0")] {
        settles(
            &b.sql,
            &count(table, "venue_id", &venue),
            rows,
            &format!("{table} on B after A deleted the node"),
        )
        .await;
    }
    let refused: Vec<(String, String, String)> =
        sqlx::query_as("SELECT table_name, row_id, op FROM sync_rejections")
            .fetch_all(&b.sql)
            .await
            .expect("refusals");
    assert_eq!(
        refused,
        [(
            "venue_node_params".to_owned(),
            param.clone(),
            "PUT".to_owned()
        )],
        "B's parameter is one refused write"
    );
    let kept: i64 =
        sqlx::query_scalar("select count(*) from public.venue_node_params where id = $1")
            .bind(&param)
            .fetch_one(&admin)
            .await
            .expect("the server's parameters");
    assert_eq!(kept, 0, "the server took a parameter under a deleted node");

    a.close().await;
    b.close().await;
}

/// A parent and its children leave a device in one checkpoint. Locally a
/// child's foreign key is real, so a parent removed in an earlier checkpoint
/// than its child, or a child that outlives it, is a download the device
/// refuses whole. First a node and a group go with what hangs off them, then
/// the venue with everything in it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the PowerSync test containers; see deploy/test-stack/run.py"]
async fn a_parent_and_its_children_deleted_together_arrive_without_a_refusal() {
    let Some(containers) = Containers::from_env() else {
        eprintln!("skipping: LUMA_TEST_POWERSYNC_URL / _POSTGREST_URL / _JWT_SECRET unset");
        return;
    };
    let admin = admin(&containers).await;
    let directory = tempfile::tempdir().expect("temp dir");
    let a = device(&directory.path().join("a"), &containers, USER).await;
    let b = device(&directory.path().join("b"), &containers, USER).await;

    let venue = unique("venue");
    let score = unique("score");
    seed(&a.sql, USER, &venue, &score, &[("one", 0.0)]).await;
    let rig = rig(&a.sql, USER, &venue).await;
    let holds = |expected: [(&'static str, &'static str); 7], why: &'static str| {
        let venue = venue.clone();
        let pool = b.sql.clone();
        async move {
            for (table, rows) in expected {
                settles(
                    &pool,
                    &count(table, "venue_id", &venue),
                    rows,
                    &format!("{table} on B {why}"),
                )
                .await;
            }
            assert_eq!(refused(&pool).await, DOWNLOAD_APPLIES, "{why}");
        }
    };
    holds(
        [
            ("venue_nodes", "2"),
            ("venue_node_params", "1"),
            ("venue_constraints", "1"),
            ("fixtures", "1"),
            ("fixture_groups", "1"),
            ("fixture_group_members", "1"),
            ("scores", "1"),
        ],
        "before anything is deleted",
    )
    .await;

    // A node with its parameter and constraint, and a group with its member.
    for (table, id) in [("venue_nodes", &rig.child), ("fixture_groups", &rig.group)] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE id = ?"
        )))
        .bind(id)
        .execute(&a.sql)
        .await
        .expect("delete a parent");
    }
    wait_for_upload(&a.sql).await;
    holds(
        [
            ("venue_nodes", "1"),
            ("venue_node_params", "0"),
            ("venue_constraints", "0"),
            ("fixtures", "1"),
            ("fixture_groups", "0"),
            ("fixture_group_members", "0"),
            ("scores", "1"),
        ],
        "after a node and a group were deleted",
    )
    .await;
    assert!(flag(&admin, "venue_nodes", &rig.child).await.0);

    // The venue, with the stage, the patch and the score in it.
    sqlx::query("DELETE FROM venues WHERE id = ?")
        .bind(&venue)
        .execute(&a.sql)
        .await
        .expect("delete the venue");
    wait_for_upload(&a.sql).await;
    holds(
        [
            ("venue_nodes", "0"),
            ("venue_node_params", "0"),
            ("venue_constraints", "0"),
            ("fixtures", "0"),
            ("fixture_groups", "0"),
            ("fixture_group_members", "0"),
            ("scores", "0"),
        ],
        "after the venue was deleted",
    )
    .await;
    settles(
        &b.sql,
        &count("venues", "id", &venue),
        "0",
        "B kept the deleted venue",
    )
    .await;
    settles(
        &b.sql,
        &count("clips", "score_id", &score),
        "0",
        "B kept the deleted venue's clips",
    )
    .await;
    for (table, id) in [
        ("venues", &venue),
        ("fixtures", &rig.fixture),
        ("scores", &score),
    ] {
        assert!(
            flag(&admin, table, id).await.0,
            "the server did not flag the venue's {table}"
        );
    }

    a.close().await;
    b.close().await;
}

/// One PostgREST read as `user`.
async fn get(containers: &Containers, user: &str, path: &str) -> serde_json::Value {
    rest(containers, user, reqwest::Method::GET, path, None).await
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

/// The data format this build reads, which every request of its says: the
/// server turns away a write that does not.
const FORMAT: i64 = crate::database::local::database::PROJECTION_VERSION;

/// One PostgREST call whose refusal is the answer, as this build makes it.
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
        .header("Authorization", format!("Bearer {token}"))
        .header(super::supabase::FORMAT_HEADER, FORMAT);
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send().await.expect("postgrest");
    let status = response.status();
    (status, response.text().await.unwrap_or_default())
}

/// A row id as the app writes one: random, and saying nothing about the row.
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A fresh name that says what it is for, so two runs against a kept stack
/// do not collide and a failure is readable.
fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    )
}

/// Wait until this device has nothing left to upload, and the server took
/// all of it.
async fn wait_for_upload(pool: &SqlitePool) {
    drained(pool).await;
    let refused = not_saved(pool).await;
    assert!(refused.is_empty(), "the server refused {refused:?}");
}

/// Wait until this device has nothing left to upload, refused or not.
async fn drained(pool: &SqlitePool) {
    until(
        pool,
        "SELECT CAST(COUNT(*) AS TEXT) FROM ps_crud",
        "the upload queue never drained",
        |count| count == "0",
    )
    .await;
}

/// `public.join_venue(code)` over PostgREST.
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
        .header(super::supabase::FORMAT_HEADER, FORMAT)
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .expect("join_venue");
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert!(status.is_success(), "join_venue {status}: {body}");
    serde_json::from_str::<String>(body.trim()).expect("join_venue returns the venue id")
}
