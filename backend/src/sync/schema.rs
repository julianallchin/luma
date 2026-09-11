//! The synced tables and the PowerSync raw-table statements they map onto.
//!
//! PowerSync raw tables write the application's own tables directly: a download
//! runs the table's put statement, a remote delete runs its delete statement.
//! There is no shadow schema and no projection step.
//!
//! Local-only tables are absent by construction — a table not listed here is
//! never uploaded, never downloaded and never cleared. So are local-only
//! columns (`tracks.file_path`, `venues.controller_port`, …) and the three
//! bookkeeping columns the old engine needed (`version`, `synced_at`,
//! `origin`).

use luma_sync::powersync::sdk::schema::{PendingStatement, PendingStatementValue, RawTable};

/// Every synced table and the columns that cross the wire, in wire order.
///
/// `id` is first on every table: it is the PowerSync row id, and the app
/// supplies it (Postgres never generates one). `uid` is the owner, and it is
/// uploaded explicitly so a row's RLS `with check` can be evaluated without
/// the server inferring anything.
pub const SYNCED_TABLES: &[(&str, &[&str])] = &[
    (
        "venues",
        &[
            "id",
            "uid",
            "name",
            "description",
            "share_code",
            "role",
            "environment",
            "groups_initialized",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "venue_members",
        &["id", "uid", "venue_id", "role", "created_at", "updated_at"],
    ),
    (
        "fixtures",
        &[
            "id",
            "uid",
            "venue_id",
            "universe",
            "address",
            "num_channels",
            "manufacturer",
            "model",
            "mode_name",
            "fixture_path",
            "label",
            "pos_x",
            "pos_y",
            "pos_z",
            "rot_x",
            "rot_y",
            "rot_z",
            "address_pinned",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "fixture_groups",
        &[
            "id",
            "uid",
            "venue_id",
            "name",
            "axis_lr",
            "axis_fb",
            "axis_ab",
            "movement_config",
            "display_order",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "fixture_group_members",
        &[
            "id",
            "uid",
            "fixture_id",
            "group_id",
            "head_index",
            "display_order",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "venue_nodes",
        &[
            "id",
            "uid",
            "venue_id",
            "kind",
            "catalog_ref",
            "label",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "venue_edges",
        &[
            "id",
            "uid",
            "child_id",
            "parent_id",
            "my_socket",
            "their_socket",
            "roll",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "venue_node_params",
        &[
            "id",
            "uid",
            "node_id",
            "key",
            "value",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "venue_constraints",
        &[
            "id",
            "uid",
            "node_id",
            "my_socket",
            "target_node",
            "target_socket",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "tracks",
        &[
            "id",
            "uid",
            "track_hash",
            "title",
            "artist",
            "album",
            "track_number",
            "disc_number",
            "duration_seconds",
            "storage_path",
            "album_art_mime",
            "album_art_storage_path",
            "source_type",
            "source_id",
            "source_filename",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "track_beats",
        &[
            "id",
            "uid",
            "track_id",
            "beats_json",
            "downbeats_json",
            "bpm",
            "downbeat_offset",
            "beats_per_bar",
            "processor_version",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "track_roots",
        &[
            "id",
            "uid",
            "track_id",
            "sections_json",
            "logits_storage_path",
            "processor_version",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "track_stems",
        &[
            "id",
            "uid",
            "track_id",
            "stem_name",
            "storage_path",
            "processor_version",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "track_drum_onsets",
        &[
            "id",
            "uid",
            "track_id",
            "onsets_json",
            "processor_version",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "track_bar_classifications",
        &[
            "id",
            "uid",
            "track_id",
            "classifications_json",
            "tag_order_json",
            "processor_version",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "track_genres",
        &[
            "id",
            "uid",
            "track_id",
            "genres_json",
            "labels_json",
            "processor_version",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "track_beat_validations",
        &[
            "id",
            "uid",
            "track_id",
            "track_hash",
            "grid_json",
            "processor_version",
            "verdict",
            "reason",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "scores",
        &[
            "id",
            "uid",
            "track_id",
            "venue_id",
            "name",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "clips",
        &[
            "id",
            "uid",
            "score_id",
            "graph",
            "start",
            "duration",
            "seed",
            "selection_seed",
            "selection_json",
            "z_index",
            "blend_mode",
            "inputs_json",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "score_definitions",
        &[
            "id",
            "uid",
            "score_id",
            "definition_json",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "patterns",
        &[
            "id",
            "uid",
            "name",
            "description",
            "category_id",
            "category_name",
            "is_verified",
            "author_name",
            "forked_from_id",
            "score_id",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "implementations",
        &[
            "id",
            "uid",
            "pattern_id",
            "name",
            "graph_json",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "cues",
        &[
            "id",
            "uid",
            "venue_id",
            "name",
            "pattern_id",
            "args_json",
            "z_index",
            "blend_mode",
            "default_target_json",
            "execution_mode_json",
            "display_order",
            "display_x",
            "display_y",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "midi_modifiers",
        &[
            "id",
            "uid",
            "venue_id",
            "name",
            "input_json",
            "groups_json",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "midi_bindings",
        &[
            "id",
            "uid",
            "venue_id",
            "trigger_json",
            "required_modifiers_json",
            "exclusive",
            "mode_json",
            "action_json",
            "target_override_json",
            "display_order",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "agent_threads",
        &[
            "id",
            "uid",
            "agent_kind",
            "subject_kind",
            "subject_id",
            "venue_id",
            "score_id",
            "title",
            "lifecycle_state",
            "implementation_id",
            "forked_from_thread_id",
            "forked_at_message_id",
            "actor",
            "parent_thread_id",
            "parent_call_id",
            "engine",
            "model",
            "provider",
            "effort",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "agent_thread_messages",
        &[
            "id",
            "uid",
            "principal_key",
            "created_in_thread_id",
            "parent_message_id",
            "depth",
            "role",
            "parts_json",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "agent_thread_transcript_heads",
        &[
            "id",
            "uid",
            "thread_id",
            "head_message_id",
            "message_count",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "drafts",
        &[
            "id",
            "uid",
            "score_id",
            "thread_id",
            "base_json",
            "state_json",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "changes",
        &[
            "id",
            "uid",
            "table_name",
            "row_id",
            "op",
            "before_json",
            "after_json",
            "actor",
            "at",
            "created_at",
            "updated_at",
        ],
    ),
];

/// The four columns Postgres declares `boolean` while SQLite stores 0/1.
///
/// They need coercion in both directions. PostgREST will not accept a JSON `0`
/// for a boolean, and `CAST('true' AS INTEGER)` is 0 in SQLite — so neither end
/// can be left to convert implicitly. See `triggers.rs` for the upload half.
pub const BOOLEAN_COLUMNS: &[&str] = &[
    "groups_initialized",
    "address_pinned",
    "is_verified",
    "exclusive",
];

/// How a composite-key table's `id` is built.
///
/// `id` is a real, client-supplied column on every synced table — PowerSync
/// addresses a row by one id, and Postgres stores exactly what the client
/// wrote. This is only the *recipe* the domain code follows when it inserts
/// such a row; nothing here writes it. `venue_edges.id` is `child_id`, a
/// per-track analysis row's `id` is its `track_id`, and the rest join their key
/// columns with `:`.
pub const COMPOSITE_IDS: &[(&str, &[&str])] = &[
    ("venue_edges", &["child_id"]),
    ("venue_node_params", &["node_id", "key"]),
    ("venue_constraints", &["node_id", "my_socket"]),
    ("track_beats", &["track_id"]),
    ("track_roots", &["track_id"]),
    ("track_stems", &["track_id", "stem_name"]),
    ("track_drum_onsets", &["track_id"]),
    ("track_bar_classifications", &["track_id"]),
    ("track_genres", &["track_id"]),
    ("track_beat_validations", &["track_id"]),
    ("agent_thread_transcript_heads", &["thread_id"]),
];

/// The `id` of a composite-key row, or `None` for a table that mints its own.
#[must_use]
pub fn composite_id(table: &str, key: &[&str]) -> Option<String> {
    COMPOSITE_IDS
        .iter()
        .find(|(name, columns)| *name == table && columns.len() == key.len())
        .map(|_| key.join(":"))
}

/// The synced columns of one table, or `None` if the table is local-only.
#[must_use]
pub fn columns(table: &str) -> Option<&'static [&'static str]> {
    SYNCED_TABLES
        .iter()
        .find(|(name, _)| *name == table)
        .map(|(_, columns)| *columns)
}

/// How a raw-table parameter must be coerced on the way in.
///
/// Raw-table put parameters arrive as text from the sync protocol. SQLite would
/// store `"3"` in an `INTEGER` column as text and then compare it unequal to
/// the `3` a local write put there, so numeric columns cast explicitly. The
/// four boolean columns cannot cast at all — Postgres sends `true`, and
/// `CAST('true' AS INTEGER)` is `0` — so they match against the set of
/// truthy spellings instead.
#[must_use]
pub fn column_expression(column: &str) -> &'static str {
    match column {
        // Postgres declares these four `boolean` and sends JSON true/false.
        // `CAST('true' AS INTEGER)` is 0, so match the value instead.
        column if BOOLEAN_COLUMNS.contains(&column) => {
            "CASE WHEN ? IN (1, '1', 'true', 'TRUE', 't') THEN 1 ELSE 0 END"
        }
        "universe" | "address" | "num_channels" | "track_number" | "disc_number" | "head_index"
        | "display_order" | "display_x" | "display_y" | "z_index" | "processor_version"
        | "beats_per_bar" | "depth" | "message_count" => "CAST(? AS INTEGER)",
        "pos_x" | "pos_y" | "pos_z" | "rot_x" | "rot_y" | "rot_z" | "axis_lr" | "axis_fb"
        | "axis_ab" | "roll" | "value" | "duration_seconds" | "bpm" | "downbeat_offset"
        | "start" | "duration" => "CAST(? AS REAL)",
        _ => "?",
    }
}

/// The put and delete statements for one synced table.
///
/// Split out of [`raw_tables`] so a test can read the SQL without building a
/// `RawTable`. Both address the row by `id`: every synced table has a real,
/// client-supplied `id text primary key` on both ends, so a download is an
/// upsert keyed on the same value the upload wrote.
#[must_use]
pub fn statements(table: &str, columns: &[&str]) -> (String, String) {
    let values = columns
        .iter()
        .map(|column| column_expression(column))
        .collect::<Vec<_>>()
        .join(", ");
    let assignments = columns
        .iter()
        .filter(|column| **column != "id")
        .map(|column| format!("\"{column}\" = excluded.\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let names = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    (
        format!(
            "INSERT INTO {table} ({names}) VALUES ({values}) \
             ON CONFLICT(id) DO UPDATE SET {assignments}"
        ),
        format!("DELETE FROM {table} WHERE id = ?"),
    )
}

/// Every synced table as a PowerSync raw table.
///
/// `clear` empties the table: `powersync_clear` runs on sign-out and on a
/// forced re-sync, and what it is clearing is another account's rows.
#[must_use]
pub fn raw_tables() -> Vec<RawTable> {
    SYNCED_TABLES
        .iter()
        .map(|(table, columns)| {
            let (put, delete) = statements(table, columns);
            let params = columns
                .iter()
                .copied()
                .map(|column| {
                    if column == "id" {
                        PendingStatementValue::Id
                    } else {
                        PendingStatementValue::Column(column.into())
                    }
                })
                .collect();
            let mut raw = RawTable::with_statements(
                *table,
                PendingStatement {
                    sql: put.into(),
                    params,
                },
                PendingStatement {
                    sql: delete.into(),
                    params: vec![PendingStatementValue::Id],
                },
            );
            raw.clear = Some(format!("DELETE FROM {table}").into());
            raw
        })
        .collect()
}

/// The PowerSync schema: raw tables only. Luma has no managed views.
#[must_use]
pub fn schema() -> luma_sync::powersync::sdk::schema::Schema {
    luma_sync::powersync::sdk::schema::Schema {
        tables: Vec::new(),
        raw_tables: raw_tables(),
    }
}
