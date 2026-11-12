//! The synced tables, named once, and the PowerSync statements they generate.
//!
//! A column a table has but this list omits is local. One list drives both
//! consumers — the upload queue in [`super::triggers`] and the raw tables a
//! download is applied through — so they cannot describe different tables. A table not listed here is never uploaded, never
//! downloaded and never cleared.

use luma_sync::powersync::sdk::schema::{PendingStatement, PendingStatementValue, RawTable};

/// What a synced table holds, which decides what a delete of its row means
/// to the server (docs/design/data-layer.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// What a person or an agent made. Its id is a random UUID the writer
    /// generates, and the server never deletes it: a delete uploads as a flag.
    Source,
    /// Computed from audio. A cache, deleted for real and rebuilt.
    Derived,
    /// Private working state, deleted for real.
    Scratch,
}

/// One synced table.
pub struct SyncedTable {
    pub name: &'static str,
    pub kind: Kind,
    /// SQL for the row's id in terms of a trigger's `NEW`/`OLD` row. A source
    /// table stores it. A derived table spells it as the concatenation its
    /// generated `id` column uses, because a generated column is not reliably
    /// readable from a trigger.
    pub id: &'static str,
    /// The column holding the owner. Every synced table spells it `uid`.
    pub uid: &'static str,
    /// The columns that travel, in schema order. A derived table omits `id`:
    /// locally it is a `GENERATED ALWAYS` column, so nothing may write it, and
    /// it is reconstructed from the key columns that are here.
    pub columns: &'static [&'static str],
    /// The columns that name "the one for this parent and key" on a table
    /// whose rows are also found that way: a node's parameter by its key, a
    /// thread's transcript head. Unique locally, so a download of a row whose
    /// slot another id holds takes the slot over. Empty for a table found by
    /// id alone.
    pub slot: &'static [&'static str],
    /// Local-only `NOT NULL` columns a download has to fill, and the SQL that
    /// fills them. A `?` in the expression binds the row's id.
    ///
    /// There are two, and they are the same fact twice: where the bytes are on
    /// *this* machine. A track that arrives from another device has no bytes
    /// here, and the app's word for that is a `.stub` path — the one thing a
    /// downloaded row can honestly say. Only an insert writes them; a later
    /// download of the same row must not undo a real path this device earned
    /// by actually fetching the file.
    pub local_defaults: &'static [(&'static str, &'static str)],
}

impl SyncedTable {
    /// The row's id, read off the trigger's `NEW` or `OLD` row.
    #[must_use]
    pub fn id_of(&self, row: &str) -> String {
        self.id.replace('@', row)
    }

    /// Whether `id` is a real, writable column on this table.
    #[must_use]
    pub fn id_is_stored(&self) -> bool {
        self.columns.contains(&"id")
    }

    /// The columns the row's `id` is built from: `id` itself where it is
    /// stored, a derived table's natural key otherwise. The conflict target a
    /// download upserts on. Read straight off [`Self::id`] so the two cannot
    /// disagree.
    #[must_use]
    pub fn key(&self) -> Vec<&'static str> {
        self.id
            .split("||")
            .map(str::trim)
            .filter_map(|part| part.strip_prefix("@."))
            .collect()
    }

    /// `json_object('column', <row>.column, ...)` for one side of a change.
    ///
    /// `id` leads whether or not it is stored: the server's copy of the row has
    /// one and a derived table's local copy computes it. The boolean
    /// columns are 0/1 integers in SQLite and `boolean` in Postgres, and
    /// PostgREST refuses a JSON `0` for a boolean — so they are injected as
    /// real JSON `true`/`false`.
    #[must_use]
    pub fn json_object(&self, row: &str) -> String {
        let mut pairs = Vec::with_capacity(self.columns.len() + 1);
        if !self.id_is_stored() {
            pairs.push(format!("'id', {}", self.id_of(row)));
        }
        for column in self.columns {
            if BOOLEAN_COLUMNS.contains(column) {
                pairs.push(format!(
                    "'{column}', json(CASE WHEN {row}.\"{column}\" THEN 'true' ELSE 'false' END)"
                ));
            } else {
                pairs.push(format!("'{column}', {row}.{column}"));
            }
        }
        format!("json_object({})", pairs.join(", "))
    }

    /// This table, with the columns that name a row's [`Self::slot`].
    const fn slot(mut self, columns: &'static [&'static str]) -> Self {
        self.slot = columns;
        self
    }
}

macro_rules! table {
    ($name:literal, $kind:ident, $id:literal, $uid:literal, [$($column:literal),* $(,)?]) => {
        table!($name, $kind, $id, $uid, [$($column),*], [])
    };
    ($name:literal, $kind:ident, $id:literal, $uid:literal, [$($column:literal),* $(,)?],
     [$(($local:literal, $fill:literal)),* $(,)?]) => {
        SyncedTable {
            name: $name,
            kind: Kind::$kind,
            id: $id,
            uid: $uid,
            columns: &[$($column),*],
            slot: &[],
            local_defaults: &[$(($local, $fill)),*],
        }
    };
}

/// Every table PowerSync carries. History is not one of them: the server keeps
/// it, and no device needs it.
pub const SYNCED_TABLES: &[SyncedTable] = &[
    table!(
        "venues",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "name",
            "description",
            "share_code",
            "role",
            "created_at",
            "updated_at",
            "environment",
            "haze"
        ]
    ),
    table!(
        "venue_members",
        Source,
        "@.id",
        "uid",
        ["id", "uid", "venue_id", "role", "created_at", "updated_at"]
    ),
    table!(
        "fixtures",
        Source,
        "@.id",
        "uid",
        [
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
            "created_at",
            "updated_at",
            "address_pinned"
        ]
    ),
    table!(
        "fixture_groups",
        Source,
        "@.id",
        "uid",
        [
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
            "updated_at"
        ]
    ),
    table!(
        "fixture_group_members",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "venue_id",
            "fixture_id",
            "group_id",
            "head_index",
            "display_order",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "venue_nodes",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "venue_id",
            "kind",
            "catalog_ref",
            "label",
            "parent_id",
            "my_socket",
            "their_socket",
            "roll",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "venue_node_params",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "node_id",
            "uid",
            "venue_id",
            "key",
            "value",
            "created_at",
            "updated_at"
        ]
    )
    .slot(&["node_id", "key"]),
    table!(
        "venue_constraints",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "node_id",
            "uid",
            "venue_id",
            "my_socket",
            "target_node",
            "target_socket",
            "created_at",
            "updated_at"
        ]
    )
    .slot(&["node_id", "my_socket"]),
    table!(
        "tracks",
        Source,
        "@.id",
        "uid",
        [
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
            "updated_at"
        ],
        [("file_path", "'downloaded/' || ? || '.stub'")]
    ),
    table!(
        "track_beats",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "track_id",
            "uid",
            "beats_json",
            "downbeats_json",
            "bpm",
            "downbeat_offset",
            "beats_per_bar",
            "processor_version",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "track_roots",
        Derived,
        "@.track_id",
        "uid",
        [
            "track_id",
            "uid",
            "sections_json",
            "logits_storage_path",
            "processor_version",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "track_stems",
        Derived,
        "@.track_id || ':' || @.stem_name",
        "uid",
        [
            "track_id",
            "uid",
            "stem_name",
            "storage_path",
            "processor_version",
            "created_at",
            "updated_at"
        ],
        [("file_path", "'downloaded/' || ? || '.stub'")]
    ),
    table!(
        "track_drum_onsets",
        Derived,
        "@.track_id",
        "uid",
        [
            "track_id",
            "uid",
            "onsets_json",
            "processor_version",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "track_bar_classifications",
        Derived,
        "@.track_id",
        "uid",
        [
            "track_id",
            "uid",
            "classifications_json",
            "tag_order_json",
            "processor_version",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "track_genres",
        Derived,
        "@.track_id",
        "uid",
        [
            "track_id",
            "uid",
            "genres_json",
            "labels_json",
            "processor_version",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "track_beat_validations",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "track_id",
            "uid",
            "track_hash",
            "grid_json",
            "processor_version",
            "verdict",
            "reason",
            "created_at",
            "updated_at"
        ]
    )
    .slot(&["track_id"]),
    table!(
        "scores",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "track_id",
            "venue_id",
            "name",
            "created_at",
            "updated_at",
            "authored_at",
            "grid_id"
        ]
    ),
    table!(
        "clips",
        Source,
        "@.id",
        "uid",
        [
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
            "name",
            "graph_json",
            "label"
        ]
    ),
    table!(
        "folders",
        Source,
        "@.id",
        "uid",
        ["id", "uid", "venue_id", "name", "created_at", "updated_at"]
    ),
    table!(
        "laser_zones",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "venue_id",
            "name",
            "turn_min",
            "turn_max",
            "tilt_min",
            "tilt_max",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "perform_patterns",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "venue_id",
            "name",
            "selection_json",
            "z_index",
            "blend_mode",
            "seed",
            "graph_json",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "perform_layouts",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "venue_id",
            "layout_json",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "perform_controllers",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "venue_id",
            "name",
            "port",
            "profile_json",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "control_bindings",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "venue_id",
            "controller_id",
            "control",
            "address_json",
            "mode",
            "invert",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "laser_limits",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "venue_id",
            "fixture_id",
            "points",
            "created_at",
            "updated_at"
        ]
    )
    .slot(&["fixture_id"]),
    table!(
        "folder_tracks",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "folder_id",
            "track_id",
            "uid",
            "venue_id",
            "created_at",
            "updated_at"
        ]
    )
    .slot(&["folder_id", "track_id"]),
    table!(
        "agent_threads",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "agent_kind",
            "subject_kind",
            "subject_id",
            "venue_id",
            "score_id",
            "title",
            "lifecycle_state",
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
            "updated_at"
        ]
    ),
    table!(
        "agent_thread_messages",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "principal_key",
            "created_in_thread_id",
            "parent_message_id",
            "depth",
            "role",
            "parts_json",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "agent_thread_transcript_heads",
        Source,
        "@.id",
        "uid",
        [
            "id",
            "thread_id",
            "uid",
            "head_message_id",
            "message_count",
            "created_at",
            "updated_at"
        ]
    )
    .slot(&["thread_id"]),
    table!(
        "drafts",
        Scratch,
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "score_id",
            "thread_id",
            "base_json",
            "state_json",
            "created_at",
            "updated_at"
        ]
    ),
];

/// One synced table by name, or `None` if it is local-only.
#[must_use]
pub fn table(name: &str) -> Option<&'static SyncedTable> {
    SYNCED_TABLES.iter().find(|table| table.name == name)
}

/// The columns Postgres declares `boolean` while SQLite stores 0/1.
///
/// They need coercion in both directions. PostgREST will not accept a JSON `0`
/// for a boolean, and `CAST('true' AS INTEGER)` is 0 in SQLite — so neither end
/// can be left to convert implicitly.
pub const BOOLEAN_COLUMNS: &[&str] = &["address_pinned", "invert"];

/// The column a `*_updated_at` trigger writes and nothing else means.
///
/// A row whose only difference is this one has not changed in any sense a
/// reader cares about: the timestamp trigger fired, or a writer rewrote the
/// row it already had. The upload queue does not record it.
pub const TOUCH_COLUMN: &str = "updated_at";

/// How a raw-table parameter must be coerced on the way in.
///
/// Raw-table put parameters arrive as text from the sync protocol. SQLite would
/// store `"3"` in an `INTEGER` column as text and then compare it unequal to
/// the `3` a local write put there, so numeric columns cast explicitly. The
/// boolean columns cannot cast at all — Postgres sends `true`, and
/// `CAST('true' AS INTEGER)` is `0` — so they match against the set of
/// truthy spellings instead.
#[must_use]
pub fn column_expression(column: &str) -> &'static str {
    match column {
        column if BOOLEAN_COLUMNS.contains(&column) => {
            "CASE WHEN ? IN (1, '1', 'true', 'TRUE', 't') THEN 1 ELSE 0 END"
        }
        "universe" | "address" | "num_channels" | "track_number" | "disc_number" | "head_index"
        | "display_order" | "z_index" | "processor_version" | "beats_per_bar" | "depth"
        | "message_count" => "CAST(? AS INTEGER)",
        "axis_lr" | "axis_fb" | "axis_ab" | "roll" | "value" | "duration_seconds" | "bpm"
        | "downbeat_offset" | "start" | "duration" => "CAST(? AS REAL)",
        _ => "?",
    }
}

/// The put and delete statements for one synced table.
///
/// Split out of [`raw_tables`] so a test can read the SQL without building a
/// `RawTable`. The delete addresses the row by `id`, which every synced table
/// has — stored, or generated with a unique index over it. The put upserts on
/// [`SyncedTable::key`]: `id` where it is stored, the natural key where it is
/// generated and cannot be written.
///
/// A table with a [`SyncedTable::slot`] upserts on that too. A checkpoint
/// that replaces a slot's row delivers the old row's removal and the new
/// row's put in no promised order, and a thread's transcript head exists
/// here, under an id of this device's making, before the server's copy
/// arrives. Either way the arriving row is the slot's row: it takes the slot
/// over, id included, and a removal of the id it replaced finds nothing.
#[must_use]
pub fn statements(table: &SyncedTable) -> (String, String) {
    let values = table
        .columns
        .iter()
        .map(|column| column_expression(column))
        .chain(table.local_defaults.iter().map(|(_, fill)| *fill))
        .collect::<Vec<_>>()
        .join(", ");
    let names = table
        .columns
        .iter()
        .chain(table.local_defaults.iter().map(|(column, _)| column))
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut put = format!(
        "INSERT INTO {} ({names}) VALUES ({values}) {}",
        table.name,
        upsert_on(table, &table.key())
    );
    if !table.slot.is_empty() {
        put.push(' ');
        put.push_str(&upsert_on(table, table.slot));
    }
    (put, format!("DELETE FROM {} WHERE id = ?", table.name))
}

/// `ON CONFLICT(target) DO UPDATE` over every travelling column but the
/// target's own.
fn upsert_on(table: &SyncedTable, target: &[&str]) -> String {
    let conflict = target
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let assignments = table
        .columns
        .iter()
        .filter(|column| !target.contains(column))
        .map(|column| format!("\"{column}\" = excluded.\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("ON CONFLICT({conflict}) DO UPDATE SET {assignments}")
}

/// Every synced table as a PowerSync raw table.
///
/// `clear` empties the table: `powersync_clear` runs on sign-out and on a
/// forced re-sync, and what it is clearing is another account's rows.
#[must_use]
pub fn raw_tables() -> Vec<RawTable> {
    SYNCED_TABLES
        .iter()
        .map(|table| {
            let (put, delete) = statements(table);
            let mut params: Vec<PendingStatementValue> = table
                .columns
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
            // Every `?` in a local default's expression is the row's id.
            params.extend(
                table
                    .local_defaults
                    .iter()
                    .flat_map(|(_, fill)| fill.matches('?'))
                    .map(|_| PendingStatementValue::Id),
            );
            let mut raw = RawTable::with_statements(
                table.name,
                PendingStatement {
                    sql: put.into(),
                    params,
                },
                PendingStatement {
                    sql: delete.into(),
                    params: vec![PendingStatementValue::Id],
                },
            );
            raw.clear = Some(format!("DELETE FROM {}", table.name).into());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_is_named_once() {
        let mut names: Vec<_> = SYNCED_TABLES.iter().map(|table| table.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "a table is listed twice");
    }

    #[test]
    fn a_composite_key_spells_the_id_its_generated_column_uses() {
        let stems = table("track_stems").expect("track_stems is synced");
        assert_eq!(stems.id_of("NEW"), "NEW.track_id || ':' || NEW.stem_name");
        assert_eq!(stems.key(), vec!["track_id", "stem_name"]);
        assert!(!stems.id_is_stored());
    }

    /// A derived table's download upserts on the natural key: `id` is
    /// generated locally, so inserting into it would fail.
    #[test]
    fn a_generated_id_is_never_written_by_a_download() {
        for synced in SYNCED_TABLES {
            let (put, delete) = statements(synced);
            assert!(
                delete.contains("WHERE id = ?"),
                "{} deletes by something other than id",
                synced.name
            );
            if synced.id_is_stored() {
                assert!(put.contains("ON CONFLICT(\"id\")"), "{}", synced.name);
            } else {
                assert!(
                    !put.contains("\"id\""),
                    "{} writes its generated id: {put}",
                    synced.name
                );
            }
        }
    }

    /// A source row's id is its writer's and travels as a column; only a
    /// cache may spell its id from the key it is cached under.
    #[test]
    fn only_a_derived_table_builds_its_id() {
        for synced in SYNCED_TABLES {
            assert_eq!(
                synced.id_is_stored(),
                synced.kind != Kind::Derived,
                "{}",
                synced.name
            );
        }
    }

    /// A device holds live rows only, so the server's flag is not a column
    /// here: a delete is a queued delete, and the connector turns it into one.
    #[test]
    fn the_delete_flag_never_travels() {
        for synced in SYNCED_TABLES {
            for flag in ["deleted_at", "deleted_by"] {
                assert!(
                    !synced.columns.contains(&flag),
                    "{} carries {flag}",
                    synced.name
                );
            }
        }
    }

    /// A download moves the rig only through tables this schema syncs; a rig
    /// table missing here would leave the rig cache stale after sync.
    #[test]
    fn every_rig_table_is_synced() {
        for name in crate::database::local::venue_graph::RIG_TABLES {
            assert!(table(name).is_some(), "{name} is not a synced table");
        }
    }

    /// Every table's `uid` travels, or the server's `with check` has nothing to
    /// evaluate a PATCH against.
    #[test]
    fn every_table_uploads_its_owner() {
        for synced in SYNCED_TABLES {
            assert!(
                synced.columns.contains(&"uid"),
                "{} does not carry uid",
                synced.name
            );
        }
    }
}
