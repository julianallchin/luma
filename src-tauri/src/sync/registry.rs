//! Static metadata for syncable tables.
//!
//! Each table declares its FK parents; the topological order — used to push
//! parents before children and pull them in the right sequence — is derived
//! once at runtime via Kahn's algorithm. This means there are no manual
//! "tier" numbers to keep in sync with the schema as tables are added.
//!
//! All pull queries rely on Supabase RLS for visibility scoping —
//! no client-side filtering needed.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::topo;

#[derive(Debug)]
pub struct TableMeta {
    pub name: &'static str,
    /// Column(s) for ON CONFLICT during upsert, and used to derive PK
    /// columns for WHERE clauses and record ID encoding.
    pub conflict_key: &'static str,
    /// Tables this row depends on via foreign key. Must reference names
    /// present in [`TABLES`]; cycles or unknown names panic at startup.
    pub parents: &'static [&'static str],
    /// Column names for the local INSERT. Order matters — binds in this order.
    pub columns: &'static [&'static str],
    /// Columns that exist locally but NOT on the remote.
    pub local_only: &'static [&'static str],
}

impl TableMeta {
    /// PK column names, split from `conflict_key`.
    pub fn pk_columns(&self) -> Vec<&str> {
        self.conflict_key.split(',').collect()
    }

    /// Whether this table has a composite primary key.
    pub fn is_composite_pk(&self) -> bool {
        self.conflict_key.contains(',')
    }

    /// Remote-only columns (excludes local_only).
    pub fn remote_columns(&self) -> Vec<&str> {
        self.columns
            .iter()
            .filter(|c| !self.local_only.contains(c))
            .copied()
            .collect()
    }

    /// Build a WHERE clause for the PK columns: `"col1 = ? AND col2 = ?"`.
    pub fn pk_where(&self) -> String {
        self.pk_columns()
            .iter()
            .map(|c| format!("{c} = ?"))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// Build `SET synced_at = updated_at, version = version + 1 WHERE pk = ?`.
    pub fn mark_synced_sql(&self) -> String {
        format!(
            "UPDATE {} SET synced_at = updated_at, version = version + 1 WHERE {}",
            self.name,
            self.pk_where()
        )
    }

    /// Build `SELECT {pk_cols} FROM {table} WHERE uid = ? AND (synced_at IS NULL OR updated_at > synced_at)`.
    /// For tables without `uid` (like fixture_group_members), omits the uid filter.
    pub fn dirty_query(&self) -> String {
        let pk_select = self.pk_columns().join(", ");
        let has_uid = self.columns.contains(&"uid");
        if has_uid {
            format!(
                "SELECT {pk_select} FROM {} WHERE uid = ? AND (synced_at IS NULL OR datetime(updated_at) > datetime(synced_at))",
                self.name
            )
        } else {
            format!(
                "SELECT {pk_select} FROM {} WHERE synced_at IS NULL OR datetime(updated_at) > datetime(synced_at)",
                self.name
            )
        }
    }

    /// Decode a record ID string into PK column values.
    pub fn decode_record_id<'a>(&self, record_id: &'a str) -> Vec<&'a str> {
        if self.is_composite_pk() {
            record_id.splitn(self.pk_columns().len(), ':').collect()
        } else {
            vec![record_id]
        }
    }
}

pub fn get_table(name: &str) -> Option<&'static TableMeta> {
    TABLES.iter().find(|t| t.name == name)
}

pub static TABLES: &[TableMeta] = &[
    TableMeta {
        name: "venues",
        conflict_key: "id",
        parents: &[],
        columns: &[
            "id",
            "uid",
            "name",
            "description",
            "share_code",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "tracks",
        conflict_key: "id",
        parents: &[],
        columns: &[
            "id",
            "uid",
            "track_hash",
            "title",
            "artist",
            "album",
            "track_number",
            "disc_number",
            "duration_seconds",
            "file_path",
            "storage_path",
            "album_art_path",
            "album_art_mime",
            "album_art_storage_path",
            "created_at",
            "updated_at",
        ],
        local_only: &["file_path", "album_art_path"],
    },
    TableMeta {
        name: "fixtures",
        conflict_key: "id",
        parents: &["venues"],
        columns: &[
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
        ],
        local_only: &[],
    },
    TableMeta {
        name: "patterns",
        conflict_key: "id",
        parents: &[],
        columns: &[
            "id",
            "uid",
            "name",
            "description",
            "category_name",
            "is_verified",
            "author_name",
            "forked_from_id",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "fixture_groups",
        conflict_key: "id",
        parents: &["venues"],
        columns: &[
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
        local_only: &[],
    },
    TableMeta {
        name: "midi_modifiers",
        conflict_key: "id",
        parents: &["venues"],
        columns: &[
            "id",
            "uid",
            "venue_id",
            "name",
            "input_json",
            "groups_json",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "implementations",
        conflict_key: "id",
        parents: &["patterns"],
        columns: &[
            "id",
            "uid",
            "pattern_id",
            "name",
            "graph_json",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "scores",
        conflict_key: "id",
        parents: &["tracks", "venues"],
        columns: &[
            "id",
            "uid",
            "track_id",
            "venue_id",
            "name",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "track_beats",
        conflict_key: "track_id",
        parents: &["tracks"],
        columns: &[
            "track_id",
            "uid",
            "bpm",
            "beats_json",
            "downbeats_json",
            "downbeat_offset",
            "beats_per_bar",
            "processor_version",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "track_roots",
        conflict_key: "track_id",
        parents: &["tracks"],
        columns: &[
            "track_id",
            "uid",
            "sections_json",
            "logits_storage_path",
            "processor_version",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    // track_waveforms excluded: local/remote schema mismatch (blob vs array columns)
    TableMeta {
        name: "track_stems",
        conflict_key: "track_id,stem_name",
        parents: &["tracks"],
        columns: &[
            "track_id",
            "uid",
            "stem_name",
            "file_path",
            "storage_path",
            "processor_version",
            "created_at",
            "updated_at",
        ],
        local_only: &["file_path"],
    },
    TableMeta {
        name: "fixture_group_members",
        conflict_key: "fixture_id,group_id",
        parents: &["fixtures", "fixture_groups"],
        columns: &[
            "id",
            "fixture_id",
            "group_id",
            "uid",
            "display_order",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "cues",
        conflict_key: "id",
        parents: &["venues", "patterns"],
        columns: &[
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
        local_only: &[],
    },
    TableMeta {
        name: "midi_bindings",
        conflict_key: "id",
        parents: &["venues"],
        columns: &[
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
        local_only: &[],
    },
    TableMeta {
        name: "track_scores",
        conflict_key: "id",
        parents: &["scores", "patterns"],
        columns: &[
            "id",
            "uid",
            "score_id",
            "pattern_id",
            "start_time",
            "end_time",
            "z_index",
            "blend_mode",
            "args_json",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
];

/// All tables in topological order (parents before children). Computed once
/// via [`crate::topo::flat`] and memoised. Pull iterates in this order; push
/// uses [`topo_position`] to sort `pending_ops` before flushing.
pub fn tables_in_topo_order() -> &'static [&'static TableMeta] {
    static ORDER: OnceLock<Vec<&'static TableMeta>> = OnceLock::new();
    ORDER.get_or_init(|| topo::flat(TABLES, |t| t.name, |t| t.parents.to_vec()))
}

/// Map of `table_name -> position in topological order`. Lower = earlier.
/// Memoised; cheap O(1) lookup for sort keys.
pub fn topo_position(name: &str) -> Option<usize> {
    static POSITIONS: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    let map = POSITIONS.get_or_init(|| {
        tables_in_topo_order()
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name, i))
            .collect()
    });
    map.get(name).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topo_order_places_parents_before_children() {
        let order = tables_in_topo_order();
        let pos: HashMap<&str, usize> =
            order.iter().enumerate().map(|(i, t)| (t.name, i)).collect();
        for t in TABLES {
            for parent in t.parents {
                assert!(
                    pos[parent] < pos[t.name],
                    "{} (idx {}) precedes its parent {} (idx {})",
                    t.name,
                    pos[t.name],
                    parent,
                    pos[parent]
                );
            }
        }
    }

    #[test]
    fn topo_position_is_consistent_with_order() {
        for (i, t) in tables_in_topo_order().iter().enumerate() {
            assert_eq!(topo_position(t.name), Some(i));
        }
        assert_eq!(topo_position("nonexistent"), None);
    }
}
