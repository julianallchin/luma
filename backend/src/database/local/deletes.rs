//! Deleting a row from a synced table.
//!
//! A delete is a delete: the row goes and the change log records it. Nothing is
//! tombstoned — the sync transport replicates the deletion itself.
//!
//! Children are removed first, by the explicit list below rather than by
//! SQLite's `ON DELETE CASCADE`. That is deliberate, and it is the same reason
//! the old engine walked them: a child's admission trigger resolves its venue
//! *through its parent*, so a cascade that has already removed the parent
//! refuses the child. Deleting bottom-up keeps every one of those checks
//! answerable.

use sqlx::SqliteConnection;

/// `parent table -> (child table, the column naming the parent's `id`)`.
///
/// Hand-maintained on purpose: a new child table is a decision about what a
/// delete means, and deriving this from the foreign keys would make that
/// decision silently.
const CHILDREN: &[(&str, &[(&str, &str)])] = &[
    (
        "venue_nodes",
        &[
            ("venue_edges", "child_id"),
            ("venue_edges", "parent_id"),
            ("venue_node_params", "node_id"),
            ("venue_constraints", "node_id"),
            ("venue_constraints", "target_node"),
        ],
    ),
    ("fixtures", &[("fixture_group_members", "fixture_id")]),
    ("fixture_groups", &[("fixture_group_members", "group_id")]),
    (
        "scores",
        &[
            ("clips", "score_id"),
            ("score_definitions", "score_id"),
            ("drafts", "score_id"),
        ],
    ),
    ("patterns", &[("implementations", "pattern_id")]),
];

/// Delete every row of `table` matching `where_sql`, and answer how many.
///
/// `where_sql` is a fragment with `?` placeholders bound from `binds`, in
/// order. The table name is a caller-owned literal, never input.
///
/// # Errors
/// Local database failures.
pub async fn delete_where(
    connection: &mut SqliteConnection,
    table: &str,
    where_sql: &str,
    binds: &[&str],
) -> Result<usize, String> {
    for (child, column) in CHILDREN
        .iter()
        .filter(|(parent, _)| *parent == table)
        .flat_map(|(_, children)| children.iter())
    {
        run(
            connection,
            &format!(
                "DELETE FROM {child} WHERE {column} IN (SELECT id FROM {table} WHERE {where_sql})"
            ),
            binds,
        )
        .await?;
    }
    run(
        connection,
        &format!("DELETE FROM {table} WHERE {where_sql}"),
        binds,
    )
    .await
}

async fn run(
    connection: &mut SqliteConnection,
    sql: &str,
    binds: &[&str],
) -> Result<usize, String> {
    let mut statement = sqlx::query(sqlx::AssertSqlSafe(sql));
    for bind in binds {
        statement = statement.bind(*bind);
    }
    let deleted = statement
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("Failed to delete: {error}"))?
        .rows_affected();
    Ok(deleted as usize)
}
