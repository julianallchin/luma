//! Unit tests for the generated SQL: the raw-table statements the SDK runs on
//! download, and the TEMP triggers that capture a local write for upload.
//!
//! Both halves are executed against a real SQLite connection, because the only
//! interesting failure mode is SQL that does not parse or does not mean what it
//! reads like.
//!
//! The tables are built here from [`SYNCED_TABLES`] rather than by running the
//! migrations: the row-model migration that creates `clips`, `drafts`,
//! `venue_members` and friends belongs to the other half of this redesign and
//! is not in the tree yet. That makes this a check of the generator, not of
//! agreement between the generator and the shipped schema — the two-device test
//! is what closes that gap.

use sqlx::{Connection, SqliteConnection};

use super::schema::{column_expression, statements, BOOLEAN_COLUMNS, SYNCED_TABLES};
use super::triggers::table_triggers;

/// A SQLite declaration for one synced table, matching what the row-model
/// migration must produce: a real `id TEXT PRIMARY KEY` the app supplies, and
/// the numeric affinities the put statement casts to.
fn create(table: &str, columns: &[&str]) -> String {
    let kind = |column: &str| {
        if BOOLEAN_COLUMNS.contains(&column) {
            "INTEGER"
        } else {
            match column_expression(column) {
                "CAST(? AS INTEGER)" => "INTEGER",
                "CAST(? AS REAL)" => "REAL",
                _ => "TEXT",
            }
        }
    };
    let definitions: Vec<String> = columns
        .iter()
        .map(|column| {
            if *column == "id" {
                "id TEXT PRIMARY KEY".to_owned()
            } else {
                format!("\"{column}\" {}", kind(column))
            }
        })
        .collect();
    format!("CREATE TABLE {table} ({});", definitions.join(", "))
}

/// A stand-in for the core extension's `powersync_crud` view, so the triggers
/// can be exercised without loading the extension.
const CRUD_QUEUE: &str =
    "CREATE TABLE powersync_crud (seq INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT, id TEXT, type TEXT, data TEXT);";

async fn database() -> SqliteConnection {
    let mut connection = SqliteConnection::connect("sqlite::memory:")
        .await
        .expect("in-memory database");
    sqlx::raw_sql(CRUD_QUEUE)
        .execute(&mut connection)
        .await
        .expect("crud queue");
    for (table, columns) in SYNCED_TABLES {
        sqlx::raw_sql(sqlx::AssertSqlSafe(create(table, columns)))
            .execute(&mut connection)
            .await
            .unwrap_or_else(|error| panic!("creating {table}: {error}"));
    }
    connection
}

/// Every generated put statement must parse, bind, and upsert rather than
/// duplicate when the same `id` arrives twice.
#[tokio::test]
async fn every_put_statement_upserts() {
    let mut connection = database().await;
    for (table, columns) in SYNCED_TABLES {
        let (put, delete) = statements(table, columns);
        for round in 0..2 {
            let mut query = sqlx::query(sqlx::AssertSqlSafe(put.clone()));
            for (index, column) in columns.iter().enumerate() {
                // The id must be stable across rounds or the second put would
                // insert rather than upsert; everything else varies.
                query = query.bind(if *column == "id" {
                    "row".to_owned()
                } else {
                    format!("{index}{round}")
                });
            }
            query
                .execute(&mut connection)
                .await
                .unwrap_or_else(|error| panic!("{table} put: {error}\n{put}"));
        }
        let rows: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&mut connection)
                .await
                .expect("count");
        assert_eq!(rows, 1, "{table} put inserted twice instead of upserting");
        sqlx::query(sqlx::AssertSqlSafe(delete.clone()))
            .bind("ignored")
            .execute(&mut connection)
            .await
            .unwrap_or_else(|error| panic!("{table} delete: {error}\n{delete}"));
    }
}

/// Every put statement writes `id` and binds one parameter per column.
#[test]
fn put_statements_write_the_client_supplied_id() {
    for (table, columns) in SYNCED_TABLES {
        let (put, _) = statements(table, columns);
        assert_eq!(
            put.matches('?').count(),
            columns.len(),
            "{table} parameter count"
        );
        assert!(put.contains("(\"id\", "), "{table} writes its id");
        assert!(put.contains("ON CONFLICT(id)"), "{table} upserts on id");
        assert!(
            !put.contains("\"id\" = excluded"),
            "{table} must not reassign its own key"
        );
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
    let mut connection = database().await;
    let (put, _) = statements("venues", super::schema::columns("venues").unwrap());
    for (sent, expected) in [("true", 1), ("false", 0), ("1", 1), ("0", 0)] {
        let mut query = sqlx::query(sqlx::AssertSqlSafe(put.clone()));
        for column in super::schema::columns("venues").unwrap() {
            query = query.bind(match *column {
                "id" => "venue-1",
                "groups_initialized" => sent,
                _ => "x",
            });
        }
        query.execute(&mut connection).await.expect("put");
        let stored: i64 =
            sqlx::query_scalar("SELECT groups_initialized FROM venues WHERE id = 'venue-1'")
                .fetch_one(&mut connection)
                .await
                .expect("read");
        assert_eq!(stored, expected, "{sent} became {stored}");
    }
}

/// An uploaded boolean leaves as JSON `true`/`false`: PostgREST will not
/// coerce a `0` into a `boolean` column.
#[tokio::test]
async fn an_uploaded_boolean_is_a_json_boolean() {
    let mut connection = database().await;
    install(&mut connection, "venues").await;
    sqlx::query(
        "INSERT INTO venues (id, uid, name, description, share_code, role, environment,
             groups_initialized, created_at, updated_at)
         VALUES ('venue-1', 'user-1', 'Room', NULL, NULL, 'owner', '{}', 0, 'now', 'now')",
    )
    .execute(&mut connection)
    .await
    .expect("insert");
    sqlx::query("UPDATE venues SET groups_initialized = 1 WHERE id = 'venue-1'")
        .execute(&mut connection)
        .await
        .expect("update");
    let entries = queue(&mut connection).await;
    let put: serde_json::Value =
        serde_json::from_str(entries[0].3.as_deref().expect("put")).expect("json");
    assert_eq!(put["groups_initialized"], serde_json::Value::Bool(false));
    let patch: serde_json::Value =
        serde_json::from_str(entries[1].3.as_deref().expect("patch")).expect("json");
    assert_eq!(
        patch,
        serde_json::json!({ "groups_initialized": true }),
        "a PATCH keeps the JSON boolean through re-aggregation"
    );
}

async fn install(connection: &mut SqliteConnection, table: &str) {
    let columns = super::schema::columns(table).expect("a synced table");
    for statement in table_triggers(table, columns) {
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(&mut *connection)
            .await
            .expect("trigger");
    }
}

async fn queue(connection: &mut SqliteConnection) -> Vec<(String, String, String, Option<String>)> {
    sqlx::query_as("SELECT op, id, type, data FROM powersync_crud ORDER BY seq")
        .fetch_all(connection)
        .await
        .expect("queue")
}

/// The whole lifecycle of one clip, as the upload queue sees it.
#[tokio::test]
async fn a_clip_insert_update_and_delete_become_put_patch_and_delete() {
    let mut connection = database().await;
    install(&mut connection, "clips").await;

    sqlx::query(
        "INSERT INTO clips (id, uid, score_id, graph, start, duration, seed, selection_seed,
             selection_json, z_index, blend_mode, inputs_json, created_at, updated_at)
         VALUES ('clip-1', 'user-1', 'score-1', '{}', 0.0, 8.0, '7', NULL, '[]', 0, 'normal',
             '{}', 'now', 'now')",
    )
    .execute(&mut connection)
    .await
    .expect("insert");
    sqlx::query("UPDATE clips SET duration = 16.0, updated_at = 'later' WHERE id = 'clip-1'")
        .execute(&mut connection)
        .await
        .expect("update");
    sqlx::query("DELETE FROM clips WHERE id = 'clip-1'")
        .execute(&mut connection)
        .await
        .expect("delete");

    let entries = queue(&mut connection).await;
    let ops: Vec<&str> = entries.iter().map(|entry| entry.0.as_str()).collect();
    assert_eq!(ops, ["PUT", "PATCH", "DELETE"]);
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
    assert_eq!(
        patch,
        serde_json::json!({ "duration": 16.0, "updated_at": "later" }),
        "a PATCH carries only the columns whose value changed"
    );

    assert_eq!(entries[2].3, None, "a DELETE carries no row");
}

/// An UPDATE that changes a column to NULL is still a change. Merge-patch
/// semantics would have erased the key instead of sending the null.
#[tokio::test]
async fn a_patch_can_clear_a_column() {
    let mut connection = database().await;
    install(&mut connection, "scores").await;
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name, created_at, updated_at)
         VALUES ('score-1', 'user-1', 'track-1', 'venue-1', 'Opening', 'now', 'now')",
    )
    .execute(&mut connection)
    .await
    .expect("insert");
    sqlx::query("UPDATE scores SET venue_id = NULL WHERE id = 'score-1'")
        .execute(&mut connection)
        .await
        .expect("update");

    let entries = queue(&mut connection).await;
    let patch: serde_json::Value =
        serde_json::from_str(entries[1].3.as_deref().expect("patch data")).expect("json");
    assert_eq!(patch, serde_json::json!({ "venue_id": null }));
}

/// An UPDATE that changes nothing must not enqueue anything: a save that
/// rewrites identical rows is the common case, and it should cost no traffic.
#[tokio::test]
async fn an_update_that_changes_nothing_enqueues_nothing() {
    let mut connection = database().await;
    install(&mut connection, "scores").await;
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name, created_at, updated_at)
         VALUES ('score-1', 'user-1', 'track-1', NULL, 'Opening', 'now', 'now')",
    )
    .execute(&mut connection)
    .await
    .expect("insert");
    sqlx::query("UPDATE scores SET name = 'Opening' WHERE id = 'score-1'")
        .execute(&mut connection)
        .await
        .expect("update");
    assert_eq!(queue(&mut connection).await.len(), 1);
}

/// A composite-key row carries the id its natural key spells, which is what
/// the connector filters PostgREST on and what Postgres stores.
#[tokio::test]
async fn a_composite_key_row_uploads_its_joined_id() {
    let mut connection = database().await;
    install(&mut connection, "track_stems").await;
    let id = super::schema::composite_id("track_stems", &["track-1", "drums"]).expect("recipe");
    assert_eq!(id, "track-1:drums");
    sqlx::query(
        "INSERT INTO track_stems (id, uid, track_id, stem_name, storage_path, processor_version,
             created_at, updated_at)
         VALUES (?, 'user-1', 'track-1', 'drums', 'stems/drums.flac', 3, 'now', 'now')",
    )
    .bind(&id)
    .execute(&mut connection)
    .await
    .expect("insert");
    let entries = queue(&mut connection).await;
    assert_eq!(entries[0].1, id);
}

/// Every synced table's triggers must install. A column in [`SYNCED_TABLES`]
/// that the table does not have fails here rather than at runtime.
#[tokio::test]
async fn every_table_installs_its_triggers() {
    let mut connection = database().await;
    super::triggers::install_crud_triggers(&mut connection)
        .await
        .expect("triggers");
}
