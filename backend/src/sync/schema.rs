//! The synced tables, named once.
//!
//! Everything that moves between this database and the server is listed here
//! with the columns that travel. A column a table has but this list omits is
//! local: a path on this machine, a cache, a counter nobody else can act on.
//!
//! Phase two builds the PowerSync `RawTable` put/delete statements from the
//! same list, so the change log and the upload queue cannot describe different
//! tables.

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
    /// The columns that travel, in schema order.
    pub columns: &'static [&'static str],
}

impl SyncedTable {
    /// `json_object('column', <row>.column, ...)` for one side of a change.
    #[must_use]
    pub fn json_object(&self, row: &str) -> String {
        let pairs = self
            .columns
            .iter()
            .map(|column| format!("'{column}', {row}.{column}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("json_object({pairs})")
    }

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
#[must_use]
pub fn logged_tables() -> impl Iterator<Item = &'static SyncedTable> {
    SYNCED_TABLES.iter().filter(|table| table.name != "changes")
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
        let params = SYNCED_TABLES
            .iter()
            .find(|table| table.name == "venue_node_params")
            .expect("venue_node_params is synced");
        assert_eq!(params.id_of("NEW"), "NEW.node_id || ':' || NEW.key");
    }
}
