//! Sync state: tracks the last server-issued sequence observed per principal
//! and table.
//!
//! The SQLite column is still named `last_pulled_at` because existing app
//! databases already carry it. Its value is now an opaque, versioned cursor
//! (`seq:<u64>`), never a client timestamp. Legacy timestamp values decode as
//! zero and cause one safe, idempotent replay from the start of the server
//! change stream.

use sqlx::SqlitePool;

use super::error::SyncError;

const SEQUENCE_PREFIX: &str = "seq:";

/// Get the last fully processed server sequence for a table and principal.
///
/// Returns zero for a table that has never been pulled and for the legacy
/// timestamp cursor format. The latter deliberately performs a full replay:
/// no client clock is trusted as a position in the server's history.
pub async fn get_last_pulled_seq(
    pool: &SqlitePool,
    uid: &str,
    table_name: &str,
) -> Result<u64, SyncError> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT last_pulled_at FROM sync_state WHERE uid = ? AND table_name = ?",
    )
    .bind(uid)
    .bind(table_name)
    .fetch_optional(pool)
    .await?;

    row.as_deref()
        .map(parse_cursor)
        .transpose()
        .map(Option::unwrap_or_default)
}

/// Advance a pull cursor monotonically. Replays and concurrent pull workers
/// can never move it backwards.
pub async fn advance_last_pulled_seq(
    pool: &SqlitePool,
    uid: &str,
    table_name: &str,
    sequence: u64,
) -> Result<(), SyncError> {
    let comparable_sequence = i64::try_from(sequence).map_err(|_| {
        SyncError::Parse(format!(
            "server-sequence pull cursor {sequence} exceeds PostgreSQL bigint"
        ))
    })?;
    let encoded = format!("{SEQUENCE_PREFIX}{sequence}");
    sqlx::query(
        "INSERT INTO sync_state (uid, table_name, last_pulled_at) VALUES (?, ?, ?)
         ON CONFLICT(uid, table_name) DO UPDATE SET
           last_pulled_at = CASE
             WHEN CAST(substr(sync_state.last_pulled_at, 1, 4) = 'seq:' AS INTEGER) = 1
                  AND CAST(substr(sync_state.last_pulled_at, 5) AS INTEGER) > ?
               THEN sync_state.last_pulled_at
             ELSE excluded.last_pulled_at
           END",
    )
    .bind(uid)
    .bind(table_name)
    .bind(&encoded)
    // Bind with INTEGER storage class. SQLite expressions have no column
    // affinity, so a text RHS would compare by storage class and could let a
    // smaller replay replace a larger numeric cursor.
    .bind(comparable_sequence)
    .execute(pool)
    .await?;

    Ok(())
}

fn parse_cursor(raw: &str) -> Result<u64, SyncError> {
    let Some(sequence) = raw.strip_prefix(SEQUENCE_PREFIX) else {
        return Ok(0);
    };
    sequence.parse::<u64>().map_err(|error| {
        SyncError::Parse(format!(
            "invalid server-sequence pull cursor {raw:?}: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::parse_cursor;

    #[test]
    fn legacy_client_clock_cursor_restarts_from_zero() {
        assert_eq!(parse_cursor("2026-08-02T01:02:03Z").unwrap(), 0);
        assert_eq!(parse_cursor("1970-01-01T00:00:00Z").unwrap(), 0);
    }

    #[test]
    fn sequence_cursor_is_strictly_parsed() {
        assert_eq!(parse_cursor("seq:42").unwrap(), 42);
        assert!(parse_cursor("seq:not-a-number").is_err());
    }
}
