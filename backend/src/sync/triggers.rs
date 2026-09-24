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
//! History is the server's: a Postgres trigger records every authored row
//! change. What it cannot see is *who* on this device made one — a person or a
//! model. So each entry carries the actor, read off [`ACTOR_TABLE`], as its
//! metadata, and the connector sends it with the request. An empty table is the
//! signed-in person.

use sqlx::SqliteConnection;

use super::schema::{SyncedTable, SYNCED_TABLES, TOUCH_COLUMN};

/// What a `*_updated_at` trigger writes, spelled the way the schema spells it.
const TOUCH_VALUE: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// The connection-local table naming who the open transaction writes for.
/// It holds at most one row, and only inside a transaction: [`attribute`]
/// writes it and [`unattribute`] clears it before commit, so a rollback clears
/// it too.
const ACTOR_TABLE: &str = "CREATE TEMP TABLE IF NOT EXISTS luma_actor (actor TEXT NOT NULL)";

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
        ACTOR_TABLE,
        "the actor table",
        "luma_actor",
    )
    .await?;
    for table in SYNCED_TABLES {
        for statement in upload_queue(table) {
            run(&mut *connection, &statement, "the upload queue", table.name).await?;
        }
    }
    Ok(())
}

/// Attribute the rest of this transaction's writes to `actor` — a model id or
/// an MCP client's label. Call inside a transaction, and call [`unattribute`]
/// before its commit.
///
/// # Errors
///
/// If the statement fails.
pub async fn attribute(connection: &mut SqliteConnection, actor: &str) -> Result<(), String> {
    run(
        &mut *connection,
        ACTOR_TABLE,
        "the actor table",
        "luma_actor",
    )
    .await?;
    sqlx::query("INSERT INTO temp.luma_actor (actor) VALUES (?)")
        .bind(actor)
        .execute(&mut *connection)
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to attribute the write: {error}"))
}

/// Give the connection back to the signed-in person.
///
/// # Errors
///
/// If the statement fails.
pub async fn unattribute(connection: &mut SqliteConnection) -> Result<(), String> {
    sqlx::query("DELETE FROM temp.luma_actor")
        .execute(connection)
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to clear the write's actor: {error}"))
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

/// The entry's metadata: the transaction's actor, or NULL for the person.
const ACTOR: &str = "(SELECT actor FROM temp.luma_actor LIMIT 1)";

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
             BEGIN
               INSERT INTO powersync_crud(op, id, type, data, metadata)
               VALUES ('PUT', {insert_id}, '{name}', {}, {ACTOR});
             END;",
            table.json_object("NEW")
        ),
        // The `HAVING` guard is load-bearing: `json_group_object` is an
        // aggregate, so the SELECT yields one row even when nothing changed,
        // and an empty PATCH would be an upload with no content.
        format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ps_crud_{name}_update
             AFTER UPDATE ON main.{name}
             WHEN {guard}
             BEGIN
               INSERT INTO powersync_crud(op, id, type, data, metadata)
               SELECT 'PATCH', {insert_id}, '{name}', json_group_object(n.key, CASE n.type
                        WHEN 'true' THEN json('true') WHEN 'false' THEN json('false')
                        ELSE n.value END), {ACTOR}
                 FROM json_each({new}) AS n
                 JOIN json_each({old}) AS o ON o.key = n.key
                WHERE n.value IS NOT o.value
               HAVING COUNT(*) > 0;
             END;"
        ),
        format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ps_crud_{name}_delete
             AFTER DELETE ON main.{name}
             BEGIN
               INSERT INTO powersync_crud(op, id, type, data, metadata)
               VALUES ('DELETE', {delete_id}, '{name}', NULL, {ACTOR});
             END;"
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::schema::table;

    /// The `*_updated_at` trigger's own write must not read as a second edit.
    #[test]
    fn a_timestamp_only_update_is_not_a_change() {
        let clips = table("clips").expect("clips is synced");
        let statement = upload_queue(clips)[1].clone();
        let guard = statement
            .split("WHEN ")
            .nth(1)
            .expect("the update trigger is guarded");
        assert!(
            !guard.contains("'updated_at', NEW.updated_at"),
            "the guard compares updated_at: {guard}"
        );
        assert!(guard.contains("'start', NEW.start"));
    }

    /// …and the timestamp still travels, as the value the touch trigger is
    /// about to write.
    #[test]
    fn an_update_uploads_the_timestamp_it_is_about_to_get() {
        let clips = table("clips").expect("clips is synced");
        assert!(upload_queue(clips)[1].contains(&format!("'updated_at', {TOUCH_VALUE}")));
    }

    /// A composite-key table enqueues the id its generated column spells, not
    /// `NEW.id`: a generated column is not reliably readable from a trigger.
    #[test]
    fn a_composite_key_row_uploads_its_generated_id() {
        let params = table("venue_node_params").expect("venue_node_params is synced");
        let [insert, _, delete] = upload_queue(params);
        assert!(insert.contains("'PUT', NEW.node_id || ':' || NEW.key"));
        assert!(delete.contains("'DELETE', OLD.node_id || ':' || OLD.key"));
        assert!(insert.contains("'id', NEW.node_id || ':' || NEW.key"));
    }
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
    async fn an_attributed_write_carries_its_actor_and_only_it_does() {
        let (_directory, pool) = database("actor.db").await;
        let mut connection = pool.acquire().await.unwrap();
        crud_queue(&mut connection).await;
        install(&mut connection).await.unwrap();

        let mut model = sqlx::Connection::begin(&mut *connection).await.unwrap();
        attribute(&mut model, "claude-opus-5-5").await.unwrap();
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
        attribute(&mut abandoned, "claude-opus-5-5").await.unwrap();
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
        let model = Some("claude-opus-5-5".to_owned());
        assert_eq!(
            queued,
            vec![
                ("PUT".into(), "v".into(), model.clone()),
                ("PATCH".into(), "v".into(), model.clone()),
                ("DELETE".into(), "v".into(), model),
                ("PUT".into(), "w".into(), None),
            ]
        );
    }

    /// A composite-key table is addressed by the id its generated column
    /// spells, because a generated column is not readable from a trigger.
    #[tokio::test]
    async fn a_composite_key_row_is_queued_under_its_generated_id() {
        let (_directory, pool) = database("composite.db").await;
        let mut connection = pool.acquire().await.unwrap();
        crud_queue(&mut connection).await;
        install(&mut connection).await.unwrap();
        for statement in [
            "INSERT INTO venues (id, uid, name) VALUES ('v', 'alice', 'Basement')",
            "INSERT INTO venue_nodes (id, uid, venue_id, kind) VALUES ('n', 'alice', 'v', 'venue')",
            "INSERT INTO venue_node_params (node_id, uid, key, value) VALUES ('n', 'alice', 'yaw', 1.0)",
        ] {
            sqlx::query(statement)
                .execute(&mut *connection)
                .await
                .unwrap();
        }
        let row_id: String =
            sqlx::query_scalar("SELECT id FROM powersync_crud WHERE type = 'venue_node_params'")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
        assert_eq!(row_id, "n:yaw");
        let generated: String =
            sqlx::query_scalar("SELECT id FROM venue_node_params WHERE node_id = 'n'")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
        assert_eq!(generated, row_id);
    }
}
