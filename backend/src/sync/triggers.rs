//! The two trigger sets a writer connection carries.
//!
//! Every insert, update and delete on a synced table appends one `changes` row
//! saying what the row was and what it became, and enqueues one entry in
//! `powersync_crud` for the upload. Both sets are `TEMP`, so they live on the
//! connection that created them and nowhere else — which is exactly the
//! distinction sync needs: a write made by this app is logged and uploaded, and
//! a row written by the sync SDK's own connection is neither, because a
//! download is not an edit anybody made. The migration connection is in the
//! same position: a schema change is not a user edit.
//!
//! Both sets read [`SyncedTable`], so they cannot describe different columns.
//!
//! `uid` travels in every entry. Postgres defaults it to `auth.uid()`, so
//! sending it is redundant for an insert — but an RLS `with check` on an
//! `UPDATE` is evaluated against the row as it will be, and a PATCH that never
//! mentions `uid` gives the policy nothing to check against on a row the client
//! believes it owns.
//!
//! An update whose only difference is `updated_at` is not a change. The
//! `*_updated_at` triggers in the schema perform their own `UPDATE`, which is a
//! second committed transition of the same row; without the guard below every
//! edit would log twice and upload twice. The timestamp still travels: the
//! update payload spells `updated_at` as the value the `*_updated_at` trigger
//! is about to write, which is the same `strftime(…,'now')` inside the same
//! statement.

use sqlx::SqliteConnection;

use super::schema::{logged_tables, SyncedTable, SYNCED_TABLES, TOUCH_COLUMN};

/// What a `*_updated_at` trigger writes, spelled the way the schema spells it.
const TOUCH_VALUE: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// Install both trigger sets on one writer connection.
///
/// Call this from the app pool's `after_connect` hook. Also creates
/// `session_actor`, the one-row TEMP table the change log reads the writer's
/// name out of. Empty means "nobody said" — a change with no actor is still an
/// honest change, so the column is nullable rather than defaulted to a fiction.
///
/// # Errors
///
/// If a trigger cannot be created — which means the table is missing, i.e. the
/// migration and [`SYNCED_TABLES`] have drifted.
pub async fn install(connection: &mut SqliteConnection) -> Result<(), String> {
    sqlx::query("CREATE TEMP TABLE IF NOT EXISTS session_actor (actor TEXT NOT NULL)")
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("failed to create the session actor table: {error}"))?;
    for table in logged_tables() {
        for statement in change_log(table) {
            run(&mut *connection, &statement, "the change log", table.name).await?;
        }
    }
    for table in SYNCED_TABLES {
        for statement in upload_queue(table) {
            run(&mut *connection, &statement, "the upload queue", table.name).await?;
        }
    }
    Ok(())
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

/// Name this connection's writer. The label travels into `changes.actor`.
///
/// Nothing calls this yet: a pooled connection is not a session, so naming the
/// writer means naming it per checkout, which is the agent loop's job when it
/// starts writing through one connection per turn.
#[allow(dead_code)]
pub async fn set_session_actor(
    connection: &mut SqliteConnection,
    actor: &str,
) -> Result<(), String> {
    sqlx::query("DELETE FROM session_actor")
        .execute(&mut *connection)
        .await
        .map_err(|error| error.to_string())?;
    sqlx::query("INSERT INTO session_actor (actor) VALUES (?)")
        .bind(actor)
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("failed to set the session actor: {error}"))?;
    Ok(())
}

/// `json_object(...)` of the row as an update leaves it: every column as it is,
/// except `updated_at`, which is the value the schema's touch trigger is about
/// to write.
fn updated_object(table: &SyncedTable, row: &str) -> String {
    table
        .json_object(row)
        .replace(
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

/// The three statements that log one table.
#[must_use]
pub fn change_log(table: &SyncedTable) -> [String; 3] {
    let name = table.name;
    [
        append(
            name,
            "insert",
            "",
            &table.id_of("NEW"),
            &table.uid_of("NEW"),
            "NULL",
            &table.json_object("NEW"),
        ),
        append(
            name,
            "update",
            &format!("WHEN {}", changed_beyond_the_timestamp(table)),
            &table.id_of("NEW"),
            &table.uid_of("NEW"),
            &table.json_object("OLD"),
            &updated_object(table, "NEW"),
        ),
        append(
            name,
            "delete",
            "",
            &table.id_of("OLD"),
            &table.uid_of("OLD"),
            &table.json_object("OLD"),
            "NULL",
        ),
    ]
}

fn append(
    table: &str,
    op: &str,
    guard: &str,
    row_id: &str,
    uid: &str,
    before: &str,
    after: &str,
) -> String {
    let event = match op {
        "insert" => "AFTER INSERT",
        "update" => "AFTER UPDATE",
        _ => "AFTER DELETE",
    };
    format!(
        "CREATE TEMP TRIGGER IF NOT EXISTS luma_log_{table}_{op} {event} ON {table} FOR EACH ROW {guard} BEGIN
    INSERT INTO changes (id, uid, table_name, row_id, op, before_json, after_json, actor)
    VALUES (
        lower(hex(randomblob(16))),
        COALESCE({uid}, ''),
        '{table}',
        {row_id},
        '{op}',
        {before},
        {after},
        (SELECT actor FROM temp.session_actor LIMIT 1)
    );
END"
    )
}

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
               INSERT INTO powersync_crud(op, id, type, data)
               VALUES ('PUT', {insert_id}, '{name}', {});
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
               INSERT INTO powersync_crud(op, id, type, data)
               SELECT 'PATCH', {insert_id}, '{name}', json_group_object(n.key, CASE n.type
                        WHEN 'true' THEN json('true') WHEN 'false' THEN json('false')
                        ELSE n.value END)
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
               INSERT INTO powersync_crud(op, id, type, data)
               VALUES ('DELETE', {delete_id}, '{name}', NULL);
             END;"
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::schema::table;

    #[test]
    fn a_clip_change_names_the_row_and_both_sides() {
        let clips = table("clips").expect("clips is synced");
        let [insert, update, delete] = change_log(clips);
        assert!(insert.contains("AFTER INSERT ON clips"));
        assert!(insert.contains("'insert'"));
        assert!(insert.contains("json_object('id', NEW.id"));
        assert!(update.contains("json_object('id', OLD.id"));
        assert!(delete.contains("AFTER DELETE ON clips"));
        assert!(!delete.contains("NEW."));
    }

    /// The `*_updated_at` trigger's own write must not read as a second edit,
    /// in either set.
    #[test]
    fn a_timestamp_only_update_is_not_a_change() {
        let clips = table("clips").expect("clips is synced");
        for statement in [change_log(clips)[1].clone(), upload_queue(clips)[1].clone()] {
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
                 (seq INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT, id TEXT, type TEXT, data TEXT)",
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

    /// The change log is the history store, so what it records has to be the
    /// row — both sides of it, named, with the writer — once per edit.
    #[tokio::test]
    async fn every_write_to_a_synced_table_appends_one_change() {
        let (_directory, pool) = database("changes.db").await;
        let mut connection = pool.acquire().await.unwrap();
        crud_queue(&mut connection).await;
        install(&mut connection).await.unwrap();
        set_session_actor(&mut connection, "user").await.unwrap();

        for statement in [
            "INSERT INTO venues (id, uid, name) VALUES ('v', 'alice', 'Basement')",
            "UPDATE venues SET name = 'Cellar' WHERE id = 'v'",
            "DELETE FROM venues WHERE id = 'v'",
        ] {
            sqlx::query(statement)
                .execute(&mut *connection)
                .await
                .unwrap();
        }

        let logged: Vec<(
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT op, uid, row_id, actor,
                        json_extract(before_json, '$.name'),
                        json_extract(after_json, '$.name')
                 FROM changes WHERE table_name = 'venues' ORDER BY rowid",
        )
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        let names: Vec<_> = logged
            .iter()
            .map(|(op, uid, row, actor, before, after)| {
                (
                    op.as_str(),
                    uid.as_str(),
                    row.as_str(),
                    actor.as_deref(),
                    before.as_deref(),
                    after.as_deref(),
                )
            })
            .collect();
        // Three writes, three entries: the `venues_updated_at` trigger's own
        // write differs only in the timestamp and is not an edit.
        assert_eq!(
            names,
            vec![
                ("insert", "alice", "v", Some("user"), None, Some("Basement")),
                (
                    "update",
                    "alice",
                    "v",
                    Some("user"),
                    Some("Basement"),
                    Some("Cellar")
                ),
                ("delete", "alice", "v", Some("user"), Some("Cellar"), None),
            ]
        );

        // `changes` is synced but never its own subject; logging it would log
        // the log.
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM changes WHERE table_name = 'changes'"
            )
            .fetch_one(&mut *connection)
            .await
            .unwrap(),
            0
        );
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

    /// The triggers are TEMP, so a second connection — the sync SDK's — writes
    /// without appending anything. That is what keeps a download out of this
    /// database's history.
    #[tokio::test]
    async fn a_connection_without_the_triggers_writes_no_history() {
        let (_directory, pool) = database("temp.db").await;
        let mut bare = pool.acquire().await.unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('v', 'alice', 'Quiet')")
            .execute(&mut *bare)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM changes")
                .fetch_one(&mut *bare)
                .await
                .unwrap(),
            0
        );
    }

    /// A composite-key table is addressed by the id its generated column
    /// spells, because a generated column is not readable from a trigger.
    #[tokio::test]
    async fn a_composite_key_row_is_logged_under_its_generated_id() {
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
            sqlx::query_scalar("SELECT row_id FROM changes WHERE table_name = 'venue_node_params'")
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
