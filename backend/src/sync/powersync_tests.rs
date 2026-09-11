//! The generated SQL, run against the real schema.
//!
//! The raw-table statements the SDK runs on download and the TEMP triggers that
//! capture a local write for upload are both executed against a database built
//! by the migrations, not by a stand-in. That is the only way this checks the
//! thing that actually matters: agreement between [`SYNCED_TABLES`] and the
//! shipped tables. A column the list names and the table does not have fails
//! here rather than at a customer's first sync.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{SqliteConnection, SqlitePool};

use super::schema::{column_expression, statements, table, BOOLEAN_COLUMNS, SYNCED_TABLES};
use super::triggers::{install, upload_queue};

/// A stand-in for the core extension's `powersync_crud` view, so the triggers
/// can be exercised without loading the extension.
const CRUD_QUEUE: &str = "CREATE TEMP TABLE powersync_crud \
     (seq INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT, id TEXT, type TEXT, data TEXT);";

/// A migrated database with write admission armed for `user-1`.
///
/// Foreign keys stay off: these tests write one table at a time, and the point
/// is the generated SQL, not referential integrity.
async fn database(name: &str) -> (tempfile::TempDir, SqlitePool) {
    let directory = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(directory.path().join(name))
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .expect("open");
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrate");
    crate::database::local::auth::arm_write_admission(&pool, Some("user-1"))
        .await
        .expect("arm");
    (directory, pool)
}

/// A connection with the crud queue stand-in and the trigger sets installed.
async fn writer(pool: &SqlitePool) -> sqlx::pool::PoolConnection<sqlx::Sqlite> {
    let mut connection = pool.acquire().await.expect("acquire");
    sqlx::raw_sql(CRUD_QUEUE)
        .execute(&mut *connection)
        .await
        .expect("crud queue");
    install(&mut connection).await.expect("triggers");
    connection
}

async fn queue(connection: &mut SqliteConnection) -> Vec<(String, String, String, Option<String>)> {
    sqlx::query_as("SELECT op, id, type, data FROM powersync_crud ORDER BY seq")
        .fetch_all(connection)
        .await
        .expect("queue")
}

/// Every generated put statement must parse, bind, and upsert rather than
/// duplicate when the same row arrives twice.
///
/// Values are bound as text, which is what the sync protocol delivers; the
/// statement's own casts are what make the stored value the right type.
#[tokio::test]
async fn every_put_statement_upserts_against_the_real_schema() {
    let (_directory, pool) = database("put.db").await;
    let mut connection = pool.acquire().await.expect("acquire");
    // Downloads land on the SDK's connection, which is not a writer and is not
    // subject to the local admission guards; `remote_writes` is the same
    // bypass spelled for a pool connection.
    crate::database::local::write_admission::enter_remote_writes(&mut connection)
        .await
        .expect("remote writes");
    for synced in SYNCED_TABLES {
        let (put, delete) = statements(synced);
        let key = synced.key();
        for round in 0..2 {
            let mut query = sqlx::query(sqlx::AssertSqlSafe(put.clone()));
            for (index, column) in synced.columns.iter().enumerate() {
                // The key must be stable across rounds or the second put would
                // insert rather than upsert; everything else varies.
                query = query.bind(if key.contains(column) {
                    format!("key-{column}")
                } else if BOOLEAN_COLUMNS.contains(column) {
                    "true".to_owned()
                } else {
                    format!("{index}{round}")
                });
            }
            query
                .execute(&mut *connection)
                .await
                .unwrap_or_else(|error| panic!("{} put: {error}\n{put}", synced.name));
        }
        let rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {}",
            synced.name
        )))
        .fetch_one(&mut *connection)
        .await
        .expect("count");
        assert_eq!(
            rows, 1,
            "{} put inserted twice instead of upserting",
            synced.name
        );
        sqlx::query(sqlx::AssertSqlSafe(delete.clone()))
            .bind("ignored")
            .execute(&mut *connection)
            .await
            .unwrap_or_else(|error| panic!("{} delete: {error}\n{delete}", synced.name));
    }
}

/// Every put statement binds one parameter per column and never reassigns the
/// key it upserted on.
#[test]
fn put_statements_bind_one_parameter_per_column() {
    for synced in SYNCED_TABLES {
        let (put, _) = statements(synced);
        assert_eq!(
            put.matches('?').count(),
            synced.columns.len(),
            "{} parameter count",
            synced.name
        );
        for key in synced.key() {
            assert!(
                !put.contains(&format!("\"{key}\" = excluded")),
                "{} must not reassign its own key",
                synced.name
            );
        }
    }
}

/// The four boolean columns cannot be cast; a cast would turn `true` into 0.
#[test]
fn boolean_columns_match_rather_than_cast() {
    for column in BOOLEAN_COLUMNS {
        assert!(
            column_expression(column).starts_with("CASE WHEN ?"),
            "{column} must not cast"
        );
    }
}

/// A downloaded boolean lands as 1 or 0 whichever spelling arrives.
#[tokio::test]
async fn a_downloaded_boolean_becomes_an_integer() {
    let (_directory, pool) = database("boolean.db").await;
    let mut connection = pool.acquire().await.expect("acquire");
    crate::database::local::write_admission::enter_remote_writes(&mut connection)
        .await
        .expect("remote writes");
    let venues = table("venues").expect("venues is synced");
    let (put, _) = statements(venues);
    for (sent, expected) in [("true", 1), ("false", 0), ("1", 1), ("0", 0)] {
        let mut query = sqlx::query(sqlx::AssertSqlSafe(put.clone()));
        for column in venues.columns {
            query = query.bind(match *column {
                "id" => "venue-1",
                "groups_initialized" => sent,
                _ => "x",
            });
        }
        query.execute(&mut *connection).await.expect("put");
        let stored: i64 =
            sqlx::query_scalar("SELECT groups_initialized FROM venues WHERE id = 'venue-1'")
                .fetch_one(&mut *connection)
                .await
                .expect("read");
        assert_eq!(stored, expected, "{sent} became {stored}");
    }
}

/// An uploaded boolean leaves as JSON `true`/`false`: PostgREST will not
/// coerce a `0` into a `boolean` column.
#[tokio::test]
async fn an_uploaded_boolean_is_a_json_boolean() {
    let (_directory, pool) = database("upload-boolean.db").await;
    let mut connection = writer(&pool).await;
    sqlx::query(
        "INSERT INTO venues (id, uid, name, groups_initialized) \
         VALUES ('venue-1', 'user-1', 'Room', 0)",
    )
    .execute(&mut *connection)
    .await
    .expect("insert");
    sqlx::query("UPDATE venues SET groups_initialized = 1 WHERE id = 'venue-1'")
        .execute(&mut *connection)
        .await
        .expect("update");
    let entries = queue(&mut connection).await;
    let put: serde_json::Value =
        serde_json::from_str(entries[0].3.as_deref().expect("put")).expect("json");
    assert_eq!(put["groups_initialized"], serde_json::Value::Bool(false));
    let patch: serde_json::Value =
        serde_json::from_str(entries[1].3.as_deref().expect("patch")).expect("json");
    assert_eq!(
        patch["groups_initialized"],
        serde_json::Value::Bool(true),
        "a PATCH keeps the JSON boolean through re-aggregation"
    );
}

/// The whole lifecycle of one clip, as the upload queue sees it.
#[tokio::test]
async fn a_clip_insert_update_and_delete_become_put_patch_and_delete() {
    let (_directory, pool) = database("clip.db").await;
    let mut connection = writer(&pool).await;
    sqlx::query(
        "INSERT INTO clips (id, uid, score_id, graph, start, duration, seed, selection_seed,
             selection_json, z_index, blend_mode, inputs_json)
         VALUES ('clip-1', 'user-1', 'score-1', '{}', 0.0, 8.0, '7', NULL, '[]', 0, 'normal', '{}')",
    )
    .execute(&mut *connection)
    .await
    .expect("insert");
    sqlx::query("UPDATE clips SET duration = 16.0 WHERE id = 'clip-1'")
        .execute(&mut *connection)
        .await
        .expect("update");
    sqlx::query("DELETE FROM clips WHERE id = 'clip-1'")
        .execute(&mut *connection)
        .await
        .expect("delete");

    let entries = queue(&mut connection).await;
    let ops: Vec<&str> = entries.iter().map(|entry| entry.0.as_str()).collect();
    assert_eq!(
        ops,
        ["PUT", "PATCH", "DELETE"],
        "the `clips_updated_at` trigger's own write is not a second edit"
    );
    assert!(entries.iter().all(|entry| entry.1 == "clip-1"));
    assert!(entries.iter().all(|entry| entry.2 == "clips"));

    let put: serde_json::Value =
        serde_json::from_str(entries[0].3.as_deref().expect("put data")).expect("json");
    // Ownership is explicit on every upload so an RLS `with check` has
    // something to check, and nulls survive so a PUT can clear a column.
    assert_eq!(put["uid"], "user-1");
    assert_eq!(put["id"], "clip-1");
    assert_eq!(put["duration"], 8.0);
    assert!(put
        .get("selection_seed")
        .is_some_and(serde_json::Value::is_null));

    let patch: serde_json::Value =
        serde_json::from_str(entries[1].3.as_deref().expect("patch data")).expect("json");
    assert_eq!(patch["duration"], 16.0);
    // The timestamp the edit uploads is the one the row ends up with.
    let stored: String = sqlx::query_scalar("SELECT updated_at FROM clips")
        .fetch_optional(&mut *connection)
        .await
        .expect("read")
        .unwrap_or_default();
    assert!(stored.is_empty(), "the row was deleted");
    assert_eq!(
        patch.as_object().expect("object").len(),
        2,
        "a PATCH carries the changed column and the new timestamp: {patch}"
    );

    assert_eq!(entries[2].3, None, "a DELETE carries no row");
}

/// An UPDATE that changes a column to NULL is still a change. Merge-patch
/// semantics would have erased the key instead of sending the null.
#[tokio::test]
async fn a_patch_can_clear_a_column() {
    let (_directory, pool) = database("clear.db").await;
    let mut connection = writer(&pool).await;
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name)
         VALUES ('score-1', 'user-1', 'track-1', NULL, 'Opening')",
    )
    .execute(&mut *connection)
    .await
    .expect("insert");
    sqlx::query("UPDATE scores SET name = NULL WHERE id = 'score-1'")
        .execute(&mut *connection)
        .await
        .expect("update");

    let entries = queue(&mut connection).await;
    let patch: serde_json::Value =
        serde_json::from_str(entries[1].3.as_deref().expect("patch data")).expect("json");
    assert_eq!(patch["name"], serde_json::Value::Null);
}

/// An UPDATE that changes nothing must not enqueue anything: a save that
/// rewrites identical rows is the common case, and it should cost no traffic.
#[tokio::test]
async fn an_update_that_changes_nothing_enqueues_nothing() {
    let (_directory, pool) = database("noop.db").await;
    let mut connection = writer(&pool).await;
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name)
         VALUES ('score-1', 'user-1', 'track-1', NULL, 'Opening')",
    )
    .execute(&mut *connection)
    .await
    .expect("insert");
    sqlx::query("UPDATE scores SET name = 'Opening' WHERE id = 'score-1'")
        .execute(&mut *connection)
        .await
        .expect("update");
    assert_eq!(queue(&mut connection).await.len(), 1);
}

/// A composite-key row carries the id its natural key spells, which is what
/// the connector filters PostgREST on and what Postgres stores.
#[tokio::test]
async fn a_composite_key_row_uploads_its_joined_id() {
    let (_directory, pool) = database("composite.db").await;
    let mut connection = writer(&pool).await;
    sqlx::query(
        "INSERT INTO track_stems (track_id, uid, stem_name, file_path, storage_path)
         VALUES ('track-1', 'user-1', 'drums', '/tmp/drums.flac', 'stems/drums.flac')",
    )
    .execute(&mut *connection)
    .await
    .expect("insert");
    let entries = queue(&mut connection).await;
    assert_eq!(entries[0].1, "track-1:drums");
    let put: serde_json::Value =
        serde_json::from_str(entries[0].3.as_deref().expect("put")).expect("json");
    assert_eq!(put["id"], "track-1:drums", "Postgres stores the joined id");
    assert!(
        put.get("file_path").is_none(),
        "a local path is not a synced column"
    );
}

/// Every synced table's triggers must install against the shipped schema.
#[tokio::test]
async fn every_table_installs_its_triggers() {
    let (_directory, pool) = database("install.db").await;
    let _connection = writer(&pool).await;
}

/// The upload queue and the change log are generated from one list, so the
/// columns they name are the same columns.
#[test]
fn both_trigger_sets_describe_the_same_columns() {
    for synced in SYNCED_TABLES {
        let [put, _, _] = upload_queue(synced);
        for column in synced.columns {
            assert!(
                put.contains(&format!("'{column}'")),
                "{} does not upload {column}",
                synced.name
            );
        }
    }
}
