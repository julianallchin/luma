//! The upload-queue triggers a writer connection carries, enqueuing uploads in
//! `powersync_crud`.
//!
//! The set is `TEMP`, so it lives on the connection that created it and nowhere
//! else. That is the distinction sync needs: a write made by this app is
//! uploaded, while a row the SDK's own connection writes is not, because a
//! download is not an edit anybody made. A migration connection is in the same
//! position.
//!
//! `uid` travels in every upload entry. An RLS `with check` on an `UPDATE` is
//! evaluated against the row as it will be, and a PATCH that never mentions
//! `uid` gives the policy nothing to check against.
//!
//! An update whose only difference is `updated_at` is not a change: the
//! schema's `*_updated_at` triggers perform their own `UPDATE`, so without the
//! guard below every edit would upload twice.
//!
//! History is the server's: a Postgres trigger records every change to a
//! source row. What it cannot see is *who* on this device made one — a person
//! or a model — or which conversation, message and group of changes it belongs
//! to. So each entry carries its [`Attribution`], read off
//! [`ATTRIBUTION_TABLE`], as its metadata: one JSON object with the keys
//! `actor`, `thread`, `message` and `group`, each a string or `null`. The
//! connector sends them with the request. An empty table is the signed-in
//! person acting alone, and the entry's metadata is NULL.
//!
//! A write this device makes to its own copy and to nobody else's — evicting a
//! signed-out account's cache — is not an edit either, and must never reach
//! the server. [`local_only`] marks the open transaction so, and every trigger
//! stands down while the mark is there. A cascade a local delete sets off runs
//! in the same transaction, so it stands down too.

use sqlx::SqliteConnection;

use super::schema::{SyncedTable, SYNCED_TABLES, TOUCH_COLUMN};

/// What a `*_updated_at` trigger writes, spelled the way the schema spells it.
const TOUCH_VALUE: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// The connection-local table naming who the open transaction writes for and
/// what its writes belong to. It holds at most one row, and only inside a
/// transaction: [`attribute`] writes it and [`unattribute`] clears it before
/// commit, so a rollback clears it too.
const ATTRIBUTION_TABLE: &str = "CREATE TEMP TABLE IF NOT EXISTS luma_attribution \
     (actor TEXT, thread TEXT, message TEXT, change_group TEXT)";

/// Who a transaction's writes are for and what they belong to, as the
/// server's history records them. Every part is optional: a person's own
/// action has no actor and may still name its group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attribution<'a> {
    /// A model id or an MCP client's label. `None` is the signed-in person.
    pub actor: Option<&'a str>,
    /// The conversation an agent's write belongs to.
    pub thread: Option<&'a str>,
    /// The message that started the turn the write belongs to.
    pub message: Option<&'a str>,
    /// The changes made together with this one: one agent turn, or one user
    /// action. Without one the server groups by its own transaction.
    pub group: Option<&'a str>,
}

impl<'a> Attribution<'a> {
    /// A write by `actor`, belonging to nothing else.
    #[must_use]
    pub fn actor(actor: &'a str) -> Self {
        Self {
            actor: Some(actor),
            ..Self::default()
        }
    }
}

/// The connection-local mark that the open transaction's writes are this
/// device's alone. Like [`ACTOR_TABLE`] it holds a row only inside a
/// transaction: [`local_only`] writes it, [`uploading`] clears it before
/// commit, and a rollback clears it with everything else.
const LOCAL_ONLY_TABLE: &str =
    "CREATE TEMP TABLE IF NOT EXISTS luma_local_only (mark INTEGER PRIMARY KEY CHECK (mark = 1))";

/// The `WHEN` every upload trigger carries.
const UPLOADING: &str = "NOT EXISTS (SELECT 1 FROM temp.luma_local_only)";

/// Install the upload queue on one writer connection. Call this from the app
/// pool's `after_connect` hook.
///
/// # Errors
///
/// If a trigger cannot be created — which means the table is missing, i.e. the
/// migration and [`SYNCED_TABLES`] have drifted.
pub async fn install(connection: &mut SqliteConnection) -> Result<(), String> {
    run(
        &mut *connection,
        ATTRIBUTION_TABLE,
        "the attribution table",
        "luma_attribution",
    )
    .await?;
    run(
        &mut *connection,
        LOCAL_ONLY_TABLE,
        "the local-only mark",
        "luma_local_only",
    )
    .await?;
    for table in SYNCED_TABLES {
        for statement in upload_queue(table) {
            run(&mut *connection, &statement, "the upload queue", table.name).await?;
        }
    }
    Ok(())
}

/// Attribute the rest of this transaction's writes. Call inside a
/// transaction, and call [`unattribute`] before its commit.
///
/// # Errors
///
/// If the statement fails.
pub async fn attribute(
    connection: &mut SqliteConnection,
    attribution: Attribution<'_>,
) -> Result<(), String> {
    run(
        &mut *connection,
        ATTRIBUTION_TABLE,
        "the attribution table",
        "luma_attribution",
    )
    .await?;
    sqlx::query(
        "INSERT INTO temp.luma_attribution (actor, thread, message, change_group)
         VALUES (?, ?, ?, ?)",
    )
    .bind(attribution.actor)
    .bind(attribution.thread)
    .bind(attribution.message)
    .bind(attribution.group)
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(|error| format!("failed to attribute the write: {error}"))
}

/// Keep the rest of this transaction's writes on this device: nothing it
/// inserts, updates or deletes — cascades included — is queued for upload.
/// Call inside a transaction, and call [`uploading`] before its commit.
///
/// On a connection without the queue — a host that does not sync — the mark
/// is made and nothing reads it.
///
/// # Errors
///
/// If a statement fails.
pub async fn local_only(connection: &mut SqliteConnection) -> Result<(), String> {
    run(
        &mut *connection,
        LOCAL_ONLY_TABLE,
        "the local-only mark",
        "luma_local_only",
    )
    .await?;
    sqlx::query("INSERT OR IGNORE INTO temp.luma_local_only (mark) VALUES (1)")
        .execute(connection)
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to keep the write local: {error}"))
}

/// Queue this connection's writes for upload again.
///
/// # Errors
///
/// If the statement fails.
pub async fn uploading(connection: &mut SqliteConnection) -> Result<(), String> {
    sqlx::query("DELETE FROM temp.luma_local_only")
        .execute(connection)
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to resume uploading: {error}"))
}

/// Give the connection back to the signed-in person.
///
/// # Errors
///
/// If the statement fails.
pub async fn unattribute(connection: &mut SqliteConnection) -> Result<(), String> {
    sqlx::query("DELETE FROM temp.luma_attribution")
        .execute(connection)
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to clear the write's attribution: {error}"))
}

async fn run(
    connection: &mut SqliteConnection,
    statement: &str,
    what: &str,
    table: &str,
) -> Result<(), String> {
    sqlx::query(sqlx::AssertSqlSafe(statement.to_owned()))
        .execute(connection)
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to install {what} on {table}: {error}"))
}

/// `json_object(...)` of the row as an update leaves it: every column as it is,
/// except `updated_at`, which is the value the schema's touch trigger is about
/// to write.
fn updated_object(table: &SyncedTable, row: &str) -> String {
    table.json_object(row).replace(
        &format!("'{TOUCH_COLUMN}', {row}.{TOUCH_COLUMN}"),
        &format!("'{TOUCH_COLUMN}', {TOUCH_VALUE}"),
    )
}

/// The `WHEN` guard that fires only when a column other than `updated_at`
/// differs. Comparing two `json_object`s with `IS NOT` treats NULL as a value
/// rather than as unknown, which is what a column comparison has to do.
fn changed_beyond_the_timestamp(table: &SyncedTable) -> String {
    let strip = |row: &str| {
        let object = table.json_object(row);
        let pair = format!(", '{TOUCH_COLUMN}', {row}.{TOUCH_COLUMN}");
        object.replace(&pair, "")
    };
    format!("{} IS NOT {}", strip("NEW"), strip("OLD"))
}

/// The entry's metadata: the transaction's attribution as one JSON object,
/// or NULL when it has none. An absent part is a JSON `null`, never a missing
/// key, so two entries attributed alike carry the same text.
const ATTRIBUTION: &str = "(SELECT json_object('actor', actor, 'thread', thread, \
     'message', message, 'group', change_group) FROM temp.luma_attribution LIMIT 1)";

/// The three `CREATE TEMP TRIGGER` statements that fill `powersync_crud`.
///
/// An INSERT uploads the whole row as a `PUT`. An UPDATE uploads only the
/// columns whose value actually changed as a `PATCH` — the two row objects are
/// decomposed with `json_each` and joined on key, so "changed" is decided by
/// SQLite's own `IS NOT`. A DELETE uploads the id alone.
#[must_use]
pub fn upload_queue(table: &SyncedTable) -> [String; 3] {
    let name = table.name;
    let insert_id = table.id_of("NEW");
    let delete_id = table.id_of("OLD");
    let new = updated_object(table, "NEW");
    let old = table.json_object("OLD");
    let guard = changed_beyond_the_timestamp(table);
    [
        format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ps_crud_{name}_insert
             AFTER INSERT ON main.{name}
             WHEN {UPLOADING}
             BEGIN
               INSERT INTO powersync_crud(op, id, type, data, metadata)
               VALUES ('PUT', {insert_id}, '{name}', {}, {ATTRIBUTION});
             END;",
            table.json_object("NEW")
        ),
        // The `HAVING` guard is load-bearing: `json_group_object` is an
        // aggregate, so the SELECT yields one row even when nothing changed,
        // and an empty PATCH would be an upload with no content.
        format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ps_crud_{name}_update
             AFTER UPDATE ON main.{name}
             WHEN {UPLOADING} AND {guard}
             BEGIN
               INSERT INTO powersync_crud(op, id, type, data, metadata)
               SELECT 'PATCH', {insert_id}, '{name}', json_group_object(n.key, CASE n.type
                        WHEN 'true' THEN json('true') WHEN 'false' THEN json('false')
                        ELSE n.value END), {ATTRIBUTION}
                 FROM json_each({new}) AS n
                 JOIN json_each({old}) AS o ON o.key = n.key
                WHERE n.value IS NOT o.value
               HAVING COUNT(*) > 0;
             END;"
        ),
        format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ps_crud_{name}_delete
             AFTER DELETE ON main.{name}
             WHEN {UPLOADING}
             BEGIN
               INSERT INTO powersync_crud(op, id, type, data, metadata)
               VALUES ('DELETE', {delete_id}, '{name}', NULL, {ATTRIBUTION});
             END;"
        ),
    ]
}

#[cfg(test)]
mod installed {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use sqlx::SqlitePool;

    /// A stand-in for the core extension's `powersync_crud` view. The app's
    /// real pool has the extension loaded; these tests only need somewhere for
    /// the upload triggers to write.
    async fn crud_queue(connection: &mut sqlx::SqliteConnection) {
        sqlx::query(
            "CREATE TEMP TABLE IF NOT EXISTS powersync_crud
                 (seq INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT, id TEXT, type TEXT, data TEXT,
                  metadata TEXT)",
        )
        .execute(connection)
        .await
        .unwrap();
    }

    async fn database(name: &str) -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join(name))
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        (directory, pool)
    }

    /// The timestamp the edit uploads is the one the row ends up with — the
    /// touch trigger and the upload payload read the same `now`.
    #[tokio::test]
    async fn the_uploaded_timestamp_is_the_one_the_row_keeps() {
        let (_directory, pool) = database("timestamp.db").await;
        let mut connection = pool.acquire().await.unwrap();
        crud_queue(&mut connection).await;
        install(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('v', 'alice', 'Basement')")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query("UPDATE venues SET name = 'Cellar' WHERE id = 'v'")
            .execute(&mut *connection)
            .await
            .unwrap();

        let queued: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT op, json_extract(data, '$.updated_at') FROM powersync_crud
                 WHERE type = 'venues' ORDER BY rowid",
        )
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        assert_eq!(queued.len(), 2, "one PUT and one PATCH, not three");
        assert_eq!(queued[0].0, "PUT");
        assert_eq!(queued[1].0, "PATCH");
        let stored: String = sqlx::query_scalar("SELECT updated_at FROM venues WHERE id = 'v'")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(queued[1].1.as_deref(), Some(stored.as_str()));
    }

    /// An attributed transaction stamps every entry it enqueues, deletes
    /// included, and the next transaction on the same connection is the
    /// person's again — whether the attributed one committed or rolled back.
    #[tokio::test]
    async fn an_attributed_write_carries_its_attribution_and_only_it_does() {
        let (_directory, pool) = database("actor.db").await;
        let mut connection = pool.acquire().await.unwrap();
        crud_queue(&mut connection).await;
        install(&mut connection).await.unwrap();

        let turn = Attribution {
            actor: Some("claude-opus-5-5"),
            thread: Some("thread-1"),
            message: Some("message-1"),
            group: Some("group-1"),
        };
        let mut model = sqlx::Connection::begin(&mut *connection).await.unwrap();
        attribute(&mut model, turn).await.unwrap();
        for statement in [
            "INSERT INTO venues (id, uid, name) VALUES ('v', 'alice', 'Basement')",
            "UPDATE venues SET name = 'Cellar' WHERE id = 'v'",
            "DELETE FROM venues WHERE id = 'v'",
        ] {
            sqlx::query(statement).execute(&mut *model).await.unwrap();
        }
        unattribute(&mut model).await.unwrap();
        model.commit().await.unwrap();

        let mut abandoned = sqlx::Connection::begin(&mut *connection).await.unwrap();
        attribute(&mut abandoned, turn).await.unwrap();
        abandoned.rollback().await.unwrap();

        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('w', 'alice', 'Attic')")
            .execute(&mut *connection)
            .await
            .unwrap();

        let queued: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT op, id, metadata FROM powersync_crud WHERE type = 'venues' ORDER BY seq",
        )
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        let stamped: Vec<(&str, &str, Option<serde_json::Value>)> = queued
            .iter()
            .map(|(op, id, metadata)| {
                let metadata = metadata
                    .as_deref()
                    .map(|text| serde_json::from_str(text).expect("metadata is JSON"));
                (op.as_str(), id.as_str(), metadata)
            })
            .collect();
        let turn = Some(serde_json::json!({
            "actor": "claude-opus-5-5",
            "thread": "thread-1",
            "message": "message-1",
            "group": "group-1",
        }));
        assert_eq!(
            stamped,
            vec![
                ("PUT", "v", turn.clone()),
                ("PATCH", "v", turn.clone()),
                ("DELETE", "v", turn),
                ("PUT", "w", None),
            ]
        );
    }

    /// A part the write does not have is a `null`, not a missing key: an
    /// actor alone and a group alone are both whole objects.
    #[tokio::test]
    async fn an_absent_part_of_the_attribution_is_null() {
        let (_directory, pool) = database("parts.db").await;
        let mut connection = pool.acquire().await.unwrap();
        crud_queue(&mut connection).await;
        install(&mut connection).await.unwrap();

        let action = Attribution {
            group: Some("group-1"),
            ..Attribution::default()
        };
        for (id, attribution) in [("v", Attribution::actor("codex")), ("w", action)] {
            let mut write = sqlx::Connection::begin(&mut *connection).await.unwrap();
            attribute(&mut write, attribution).await.unwrap();
            sqlx::query("INSERT INTO venues (id, uid, name) VALUES (?, 'alice', 'Room')")
                .bind(id)
                .execute(&mut *write)
                .await
                .unwrap();
            unattribute(&mut write).await.unwrap();
            write.commit().await.unwrap();
        }

        let queued: Vec<String> = sqlx::query_scalar(
            "SELECT metadata FROM powersync_crud WHERE type = 'venues' ORDER BY seq",
        )
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        let queued: Vec<serde_json::Value> = queued
            .iter()
            .map(|text| serde_json::from_str(text).expect("metadata is JSON"))
            .collect();
        assert_eq!(
            queued,
            vec![
                serde_json::json!({"actor": "codex", "thread": null, "message": null, "group": null}),
                serde_json::json!({"actor": null, "thread": null, "message": null, "group": "group-1"}),
            ]
        );
    }

    /// A source row is queued under the id its writer gave it, and a derived
    /// one under the id its key spells, because a generated column is not
    /// readable from a trigger.
    #[tokio::test]
    async fn a_row_is_queued_under_its_own_id() {
        let (_directory, pool) = database("ids.db").await;
        let mut connection = pool.acquire().await.unwrap();
        crud_queue(&mut connection).await;
        install(&mut connection).await.unwrap();
        for statement in [
            "INSERT INTO venue_node_params (id, node_id, uid, key, value)
             VALUES ('0b9f', 'n', 'alice', 'yaw', 1.0)",
            "INSERT INTO track_stems (track_id, uid, stem_name, file_path)
             VALUES ('t', 'alice', 'drums', '/tmp/drums.flac')",
        ] {
            sqlx::query(statement)
                .execute(&mut *connection)
                .await
                .unwrap();
        }
        let queued: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT type, id, json_extract(data, '$.id') FROM powersync_crud ORDER BY seq",
        )
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        assert_eq!(
            queued,
            vec![
                ("venue_node_params".into(), "0b9f".into(), "0b9f".into()),
                ("track_stems".into(), "t:drums".into(), "t:drums".into()),
            ]
        );
        let generated: String =
            sqlx::query_scalar("SELECT id FROM track_stems WHERE track_id = 't'")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
        assert_eq!(generated, "t:drums");
    }
}
