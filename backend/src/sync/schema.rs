//! The synced tables, named once, and the PowerSync statements they generate.
//!
//! Everything that moves between this database and the server is listed here
//! with the columns that travel. A column a table has but this list omits is
//! local: a path on this machine, a cache, a counter nobody else can act on.
//!
//! The same list drives all three consumers, so they cannot describe different
//! tables: the change log in [`super::triggers`], the PowerSync upload queue
//! next to it, and the PowerSync raw tables a download is applied through.
//!
//! PowerSync raw tables write the application's own tables directly — there is
//! no shadow schema and no projection step. A table not listed here is never
//! uploaded, never downloaded and never cleared.

use luma_sync::powersync::sdk::schema::{PendingStatement, PendingStatementValue, RawTable};

/// One synced table.
pub struct SyncedTable {
    pub name: &'static str,
    /// SQL for the row's id in terms of a trigger's `NEW`/`OLD` row. A table
    /// with a natural composite key spells it as the concatenation its
    /// generated `id` column uses, because a generated column is not reliably
    /// readable from a trigger.
    pub id: &'static str,
    /// The column holding the owner. Every synced table spells it `uid`.
    pub uid: &'static str,
    /// The columns that travel, in schema order. A composite-key table omits
    /// `id`: locally it is a `GENERATED ALWAYS` column, so nothing may write
    /// it, and it is reconstructed from the key columns that are here.
    pub columns: &'static [&'static str],
}

impl SyncedTable {
    /// The row's id, read off the trigger's `NEW` or `OLD` row.
    #[must_use]
    pub fn id_of(&self, row: &str) -> String {
        self.id.replace('@', row)
    }

    /// The row's owner, read off the trigger's `NEW` or `OLD` row.
    #[must_use]
    pub fn uid_of(&self, row: &str) -> String {
        format!("{row}.{}", self.uid)
    }

    /// Whether `id` is a real, writable column on this table.
    #[must_use]
    pub fn id_is_stored(&self) -> bool {
        self.columns.contains(&"id")
    }

    /// The columns the row's `id` is built from — its natural key, and the
    /// conflict target a download upserts on. Read straight off [`Self::id`]
    /// so the two cannot disagree.
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
    /// one and a composite-key table's local copy computes it. The four boolean
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
}

macro_rules! table {
    ($name:literal, $id:literal, $uid:literal, [$($column:literal),* $(,)?]) => {
        SyncedTable { name: $name, id: $id, uid: $uid, columns: &[$($column),*] }
    };
}

/// Every table PowerSync carries. `changes` is on the list — a person's own
/// history follows them between devices — but it never gets a change-log
/// trigger of its own, or writing one would write another.
pub const SYNCED_TABLES: &[SyncedTable] = &[
    table!(
        "venues",
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
            "groups_initialized"
        ]
    ),
    table!(
        "venue_members",
        "@.id",
        "uid",
        ["id", "uid", "venue_id", "role", "created_at", "updated_at"]
    ),
    table!(
        "fixtures",
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
            "pos_x",
            "pos_y",
            "pos_z",
            "rot_x",
            "rot_y",
            "rot_z",
            "created_at",
            "updated_at",
            "address_pinned"
        ]
    ),
    table!(
        "fixture_groups",
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
        "@.id",
        "uid",
        [
            "id",
            "uid",
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
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "venue_id",
            "kind",
            "catalog_ref",
            "label",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "venue_edges",
        "@.child_id",
        "uid",
        [
            "child_id",
            "uid",
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
        "@.node_id || ':' || @.key",
        "uid",
        ["node_id", "uid", "key", "value", "created_at", "updated_at"]
    ),
    table!(
        "venue_constraints",
        "@.node_id || ':' || @.my_socket",
        "uid",
        [
            "node_id",
            "uid",
            "my_socket",
            "target_node",
            "target_socket",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "tracks",
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
        ]
    ),
    table!(
        "track_beats",
        "@.track_id",
        "uid",
        [
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
        ]
    ),
    table!(
        "track_drum_onsets",
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
        "@.track_id",
        "uid",
        [
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
    ),
    table!(
        "scores",
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "track_id",
            "venue_id",
            "name",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "clips",
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
            "updated_at"
        ]
    ),
    table!(
        "score_definitions",
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "score_id",
            "definition_json",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "patterns",
        "@.id",
        "uid",
        [
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
            "updated_at"
        ]
    ),
    table!(
        "implementations",
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "pattern_id",
            "name",
            "graph_json",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "cues",
        "@.id",
        "uid",
        [
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
            "updated_at"
        ]
    ),
    table!(
        "midi_modifiers",
        "@.id",
        "uid",
        [
            "id",
            "uid",
            "venue_id",
            "name",
            "input_json",
            "groups_json",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "midi_bindings",
        "@.id",
        "uid",
        [
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
            "updated_at"
        ]
    ),
    table!(
        "agent_threads",
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
            "updated_at"
        ]
    ),
    table!(
        "agent_thread_messages",
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
        "@.thread_id",
        "uid",
        [
            "thread_id",
            "uid",
            "head_message_id",
            "message_count",
            "created_at",
            "updated_at"
        ]
    ),
    table!(
        "drafts",
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
    table!(
        "changes",
        "@.id",
        "uid",
        [
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
            "updated_at"
        ]
    ),
];

/// The tables the change log records. `changes` describes the others and is
/// not its own subject.
pub fn logged_tables() -> impl Iterator<Item = &'static SyncedTable> {
    SYNCED_TABLES.iter().filter(|table| table.name != "changes")
}

/// One synced table by name, or `None` if it is local-only.
#[must_use]
pub fn table(name: &str) -> Option<&'static SyncedTable> {
    SYNCED_TABLES.iter().find(|table| table.name == name)
}

/// The four columns Postgres declares `boolean` while SQLite stores 0/1.
///
/// They need coercion in both directions. PostgREST will not accept a JSON `0`
/// for a boolean, and `CAST('true' AS INTEGER)` is 0 in SQLite — so neither end
/// can be left to convert implicitly.
pub const BOOLEAN_COLUMNS: &[&str] = &[
    "groups_initialized",
    "address_pinned",
    "is_verified",
    "exclusive",
];

/// The column a `*_updated_at` trigger writes and nothing else means.
///
/// A row whose only difference is this one has not changed in any sense a
/// reader cares about: the timestamp trigger fired, or a writer rewrote the
/// row it already had. Neither the change log nor the upload queue records it.
pub const TOUCH_COLUMN: &str = "updated_at";

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
/// `RawTable`. The delete addresses the row by `id`, which every synced table
/// has — stored, or generated with a unique index over it. The put upserts on
/// the natural key instead, because a generated `id` cannot be written.
#[must_use]
pub fn statements(table: &SyncedTable) -> (String, String) {
    let values = table
        .columns
        .iter()
        .map(|column| column_expression(column))
        .collect::<Vec<_>>()
        .join(", ");
    let key = table.key();
    let assignments = table
        .columns
        .iter()
        .filter(|column| !key.contains(column))
        .map(|column| format!("\"{column}\" = excluded.\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let names = table
        .columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let conflict = key
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let name = table.name;
    (
        format!(
            "INSERT INTO {name} ({names}) VALUES ({values}) \
             ON CONFLICT({conflict}) DO UPDATE SET {assignments}"
        ),
        format!("DELETE FROM {name} WHERE id = ?"),
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
        .map(|table| {
            let (put, delete) = statements(table);
            let params = table
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
        let params = table("venue_node_params").expect("venue_node_params is synced");
        assert_eq!(params.id_of("NEW"), "NEW.node_id || ':' || NEW.key");
        assert_eq!(params.key(), vec!["node_id", "key"]);
        assert!(!params.id_is_stored());
    }

    /// A composite-key table's download upserts on the natural key: `id` is
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
