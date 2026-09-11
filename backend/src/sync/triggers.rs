//! The change log, installed per connection.
//!
//! Every insert, update and delete on a synced table appends one `changes` row
//! saying what the row was and what it became. The triggers are `TEMP`, so they
//! live on the connection that created them and nowhere else — which is exactly
//! the distinction sync needs: a write made by this app is logged, and a row
//! written by the sync SDK's own connection is not, because a download is not
//! an edit anybody made.
//!
//! Phase two installs a second set next to these, writing PowerSync's upload
//! queue from the same [`SyncedTable`] list. [`change_log`] is the generator for
//! one table so the two sets cannot describe different columns.
//!
//! Known cost: each table's `*_updated_at` trigger performs its own `UPDATE`,
//! which is a second committed transition and so a second `changes` row whose
//! only difference is the timestamp. Removing it means setting `updated_at` in
//! every writer instead of in a trigger — worth doing, and not in this pass.

use sqlx::SqliteConnection;

use super::schema::{logged_tables, SyncedTable};

/// Install the change-log triggers on one writer connection.
///
/// Also creates `session_actor`, the one-row TEMP table the triggers read the
/// writer's name out of. Empty means "nobody said" — a change with no actor is
/// still an honest change, so the column is nullable rather than defaulted to a
/// fiction.
pub async fn install_change_log(connection: &mut SqliteConnection) -> Result<(), String> {
    sqlx::query("CREATE TEMP TABLE IF NOT EXISTS session_actor (actor TEXT NOT NULL)")
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("failed to create the session actor table: {error}"))?;
    for table in logged_tables() {
        for statement in change_log(table) {
            sqlx::query(sqlx::AssertSqlSafe(statement))
                .execute(&mut *connection)
                .await
                .map_err(|error| {
                    format!(
                        "failed to install the change log on {}: {error}",
                        table.name
                    )
                })?;
        }
    }
    Ok(())
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

/// The three statements that log one table.
#[must_use]
pub fn change_log(table: &SyncedTable) -> [String; 3] {
    let name = table.name;
    [
        append(
            name,
            "insert",
            &table.id_of("NEW"),
            &table.uid_of("NEW"),
            "NULL",
            &table.json_object("NEW"),
        ),
        append(
            name,
            "update",
            &table.id_of("NEW"),
            &table.uid_of("NEW"),
            &table.json_object("OLD"),
            &table.json_object("NEW"),
        ),
        append(
            name,
            "delete",
            &table.id_of("OLD"),
            &table.uid_of("OLD"),
            &table.json_object("OLD"),
            "NULL",
        ),
    ]
}

fn append(table: &str, op: &str, row_id: &str, uid: &str, before: &str, after: &str) -> String {
    let event = match op {
        "insert" => "AFTER INSERT",
        "update" => "AFTER UPDATE",
        _ => "AFTER DELETE",
    };
    format!(
        "CREATE TEMP TRIGGER IF NOT EXISTS luma_log_{table}_{op} {event} ON {table} FOR EACH ROW BEGIN
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::schema::SYNCED_TABLES;

    #[test]
    fn a_clip_change_names_the_row_and_both_sides() {
        let clips = SYNCED_TABLES
            .iter()
            .find(|table| table.name == "clips")
            .expect("clips is synced");
        let [insert, update, delete] = change_log(clips);
        assert!(insert.contains("AFTER INSERT ON clips"));
        assert!(insert.contains("'insert'"));
        assert!(insert.contains("json_object('id', NEW.id"));
        assert!(update.contains("json_object('id', OLD.id"));
        assert!(delete.contains("AFTER DELETE ON clips"));
        assert!(!delete.contains("NEW."));
    }
}

#[cfg(test)]
mod installed {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    /// The change log is the history store, so what it records has to be the
    /// row — both sides of it, named, with the writer.
    #[tokio::test]
    async fn every_write_to_a_synced_table_appends_one_change() {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("changes.db"))
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
        let mut connection = pool.acquire().await.unwrap();
        install_change_log(&mut connection).await.unwrap();
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
        // The `venues_updated_at` trigger's own write is a second committed
        // transition of the row, so the log has a second `update` entry whose
        // only difference is the timestamp. The log is a log; it records what
        // the database did.
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
                (
                    "update",
                    "alice",
                    "v",
                    Some("user"),
                    Some("Cellar"),
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

    /// The triggers are TEMP, so a second connection — the sync SDK's — writes
    /// without appending anything. That is what keeps a download out of this
    /// database's history.
    #[tokio::test]
    async fn a_connection_without_the_triggers_writes_no_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("temp.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
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
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("composite.db"))
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
        let mut connection = pool.acquire().await.unwrap();
        install_change_log(&mut connection).await.unwrap();
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
