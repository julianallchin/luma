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
                    format!("failed to install the change log on {}: {error}", table.name)
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
