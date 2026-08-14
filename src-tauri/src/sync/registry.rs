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

/// Remote-only, server-assigned change cursor. It is deliberately absent from
/// every table's `columns`: clients select it for pagination but never write it
/// into SQLite projections or send it back to Supabase.
pub const SERVER_CURSOR_COLUMN: &str = "sync_seq";
pub const PULL_PAGE_LIMIT: usize = 500;

/// Replicated event/content tables whose primary-key identity is immutable.
/// Supabase permits an exact response-loss replay but rejects a duplicate key
/// carrying different bytes or metadata. Heads are deliberately absent: they
/// move only through the authored proposal RPCs.
pub const IMMUTABLE_TABLES: &[&str] = &[
    "authored_documents",
    "authored_revisions",
    "authored_revision_files",
    "authored_revision_parents",
    "authored_turn_preparations",
    "authored_turn_outcomes",
    "authored_operation_outcomes",
    "agent_thread_messages",
    "agent_thread_message_appends",
    "agent_thread_deletions",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushPolicy {
    /// Discover `synced_at IS NULL`, enqueue a mutable upsert, then mark clean.
    DirtyUpsert,
    /// The product row is immutable and carries no delivery metadata. Its
    /// creating transaction explicitly enqueues `insert_immutable`; successful
    /// pending-op removal is the delivery receipt.
    ExplicitImmutable,
    /// Mutable product state with delivery metadata kept solely in
    /// `pending_ops`. The creating transaction explicitly enqueues a complete
    /// row snapshot, and a successful delivery removes only that queue row.
    ExplicitUpsert,
    /// A server-authoritative projection. Clients may select it but can only
    /// mutate it through one of the authored-head RPCs.
    ServerAuthority,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PullPolicy {
    /// Ordinary row-sync state carrying local `synced_at`/`origin` metadata.
    DirtyUpsert,
    /// An append-only row. A duplicate identity must contain identical bytes.
    Immutable,
    /// Mutable state without delivery columns (thread metadata/transcript
    /// heads and the server-authored document head projection).
    ProjectionUpsert,
    /// Thread routing is immutable and deletion is terminal. Remote `active`
    /// snapshots may update presentation metadata but never resurrect a
    /// locally observed `deleting` lifecycle.
    ThreadProjection,
    /// Immutable client input plus fields assigned exactly once by an RPC.
    /// Pull may fill those server fields, but may never rewrite the input.
    ServerEnriched,
    /// Immutable identity plus the one-way `archived_at` transition.
    TerminalArchive,
}

pub fn is_immutable_table(name: &str) -> bool {
    IMMUTABLE_TABLES.contains(&name)
}

pub fn push_policy(name: &str) -> PushPolicy {
    if is_immutable_table(name) {
        PushPolicy::ExplicitImmutable
    } else if name == "agent_threads" {
        PushPolicy::ExplicitUpsert
    } else if matches!(
        name,
        "authored_document_heads"
            | "authored_head_proposals"
            | "authored_head_integrations"
            | "authored_document_archives"
            | "agent_thread_transcript_heads"
    ) {
        PushPolicy::ServerAuthority
    } else {
        PushPolicy::DirtyUpsert
    }
}

pub fn pull_policy(name: &str) -> PullPolicy {
    match name {
        "authored_documents" => PullPolicy::TerminalArchive,
        "authored_head_proposals" | "authored_document_archives" => PullPolicy::ServerEnriched,
        "agent_threads" => PullPolicy::ThreadProjection,
        "agent_thread_transcript_heads" | "authored_document_heads" => PullPolicy::ProjectionUpsert,
        "authored_head_integrations" => PullPolicy::Immutable,
        _ if is_immutable_table(name) => PullPolicy::Immutable,
        _ => PullPolicy::DirtyUpsert,
    }
}

pub fn is_binary_column(table: &str, column: &str) -> bool {
    matches!((table, column), ("authored_revision_files", "content"))
}

/// Legacy library rows use a remote soft-delete column. Authored history and
/// conversation traces are append-only or have their own terminal archive /
/// deletion records, so requesting a synthetic `deleted_at` from those tables
/// would both fail PostgREST selection and blur the ownership model.
pub fn has_remote_tombstone(name: &str) -> bool {
    !matches!(
        name,
        "authored_documents"
            | "authored_revisions"
            | "authored_revision_files"
            | "authored_revision_parents"
            | "authored_document_heads"
            | "authored_operation_outcomes"
            | "authored_head_proposals"
            | "authored_head_integrations"
            | "authored_document_archives"
            | "agent_threads"
            | "agent_thread_messages"
            | "agent_thread_transcript_heads"
            | "agent_thread_message_appends"
            | "authored_turn_preparations"
            | "authored_turn_outcomes"
            | "agent_thread_deletions"
    )
}

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

    /// Build `SELECT {pk_cols} FROM {table} WHERE uid = ? AND synced_at IS NULL`.
    /// For tables without `uid` (like fixture_group_members), omits the uid filter.
    pub fn dirty_query(&self) -> String {
        let pk_select = self.pk_columns().join(", ");
        if self.columns.contains(&"uid") {
            format!(
                "SELECT {pk_select} FROM {} WHERE uid = ? AND synced_at IS NULL",
                self.name
            )
        } else if self.columns.contains(&"principal_key") {
            format!(
                "SELECT {pk_select} FROM {} WHERE principal_key = 'signed-in:' || ? AND synced_at IS NULL",
                self.name
            )
        } else if self.columns.contains(&"owner_user_id") {
            format!(
                "SELECT {pk_select} FROM {} WHERE owner_user_id = ? AND synced_at IS NULL",
                self.name
            )
        } else {
            format!(
                "SELECT {pk_select} FROM {} WHERE synced_at IS NULL",
                self.name
            )
        }
    }

    pub fn has_principal(&self) -> bool {
        self.columns.contains(&"uid")
            || self.columns.contains(&"principal_key")
            || self.columns.contains(&"owner_user_id")
    }

    pub fn payload_principal_matches(&self, payload: &serde_json::Value, user_id: &str) -> bool {
        if self.columns.contains(&"uid") {
            payload.get("uid").and_then(serde_json::Value::as_str) == Some(user_id)
        } else if self.columns.contains(&"principal_key") {
            payload
                .get("principal_key")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| value == format!("signed-in:{user_id}"))
        } else if self.columns.contains(&"owner_user_id") {
            payload
                .get("owner_user_id")
                .and_then(serde_json::Value::as_str)
                == Some(user_id)
        } else {
            false
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
        name: "track_drum_onsets",
        conflict_key: "track_id",
        parents: &["tracks"],
        columns: &[
            "track_id",
            "uid",
            "onsets_json",
            "processor_version",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "track_bar_classifications",
        conflict_key: "track_id",
        parents: &["tracks"],
        columns: &[
            "track_id",
            "uid",
            "classifications_json",
            "tag_order_json",
            "processor_version",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "track_genres",
        conflict_key: "track_id",
        parents: &["tracks"],
        columns: &[
            "track_id",
            "uid",
            "genres_json",
            "labels_json",
            "processor_version",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "fixture_group_members",
        // Conflict on the row UUID (not fixture_id,group_id): head-level rows
        // make that pair non-unique, and the delete trigger enqueues OLD.id.
        conflict_key: "id",
        parents: &["fixtures", "fixture_groups"],
        columns: &[
            "id",
            "fixture_id",
            "group_id",
            "head_index",
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
        name: "authored_documents",
        conflict_key: "document_id",
        parents: &[],
        columns: &[
            "document_id",
            "document_kind",
            "principal_key",
            "subject_id",
            "track_id",
            "venue_id",
            "score_id",
            "implementation_id",
            "archived_at",
            "created_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_revisions",
        conflict_key: "revision_id",
        parents: &["authored_documents"],
        columns: &[
            "revision_id",
            "document_id",
            "principal_key",
            "parent_count",
            "content_hash",
            "operation_kind",
            "operation_id",
            "message",
            "author_name",
            "author_email",
            "authored_at",
            "thread_id",
            "assistant_message_id",
            "restored_revision_id",
            "created_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_revision_files",
        conflict_key: "revision_id,path",
        parents: &["authored_revisions"],
        columns: &[
            "revision_id",
            "principal_key",
            "path",
            "content_hash",
            "content",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_revision_parents",
        conflict_key: "revision_id,parent_order",
        parents: &["authored_revisions"],
        columns: &[
            "principal_key",
            "document_id",
            "revision_id",
            "parent_order",
            "parent_revision_id",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_document_heads",
        conflict_key: "document_id",
        parents: &[
            "authored_revisions",
            "authored_revision_files",
            "authored_revision_parents",
        ],
        columns: &[
            "document_id",
            "principal_key",
            "revision_id",
            "generation",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_operation_outcomes",
        conflict_key: "document_id,operation_kind,operation_id",
        parents: &[
            "authored_revisions",
            "authored_revision_files",
            "authored_revision_parents",
        ],
        columns: &[
            "principal_key",
            "document_id",
            "operation_kind",
            "operation_id",
            "request_fingerprint",
            "base_revision_id",
            "status",
            "result_revision_id",
            "conflicts_json",
            "result_json",
            "created_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_head_proposals",
        conflict_key: "proposal_id",
        parents: &[
            "authored_revisions",
            "authored_revision_files",
            "authored_revision_parents",
        ],
        columns: &[
            "proposal_id",
            "principal_key",
            "document_id",
            "device_id",
            "operation_id",
            "base_revision_id",
            "proposed_revision_id",
            "server_proposal_seq",
            "created_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_head_integrations",
        conflict_key: "proposal_id",
        parents: &[
            "authored_head_proposals",
            "authored_revisions",
            "authored_revision_files",
            "authored_revision_parents",
            "authored_document_heads",
        ],
        columns: &[
            "proposal_id",
            "principal_key",
            "document_id",
            "prior_revision_id",
            "result_revision_id",
            "resolution_kind",
            "server_integration_seq",
            "integrated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_document_archives",
        conflict_key: "archive_id",
        parents: &[
            "authored_documents",
            "authored_revisions",
            "authored_revision_files",
            "authored_revision_parents",
        ],
        columns: &[
            "archive_id",
            "principal_key",
            "document_id",
            "device_id",
            "operation_id",
            "requested_revision_id",
            "final_revision_id",
            "server_archive_seq",
            "archived_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "agent_threads",
        conflict_key: "id",
        parents: &[],
        columns: &[
            "id",
            "owner_user_id",
            "agent_kind",
            "subject_kind",
            "subject_id",
            "implementation_id",
            "venue_id",
            "score_id",
            "title",
            "lifecycle_state",
            "forked_from_thread_id",
            "forked_at_message_id",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "agent_thread_messages",
        conflict_key: "id",
        parents: &["agent_threads", "authored_turn_preparations"],
        columns: &[
            "id",
            "owner_user_id",
            "principal_key",
            "created_in_thread_id",
            "parent_message_id",
            "depth",
            "role",
            "parts_json",
            "created_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "agent_thread_transcript_heads",
        conflict_key: "thread_id",
        parents: &["agent_threads", "agent_thread_messages"],
        columns: &[
            "thread_id",
            "owner_user_id",
            "head_message_id",
            "message_count",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "agent_thread_message_appends",
        conflict_key: "thread_id,operation_id",
        parents: &["agent_thread_messages"],
        columns: &[
            "thread_id",
            "owner_user_id",
            "principal_key",
            "operation_id",
            "request_fingerprint",
            "base_head_message_id",
            "first_message_id",
            "result_head_message_id",
            "message_count",
            "created_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_turn_preparations",
        conflict_key: "thread_id,assistant_message_id",
        parents: &[
            "agent_threads",
            "authored_revisions",
            "authored_revision_files",
            "authored_revision_parents",
        ],
        columns: &[
            "thread_id",
            "assistant_message_id",
            "owner_user_id",
            "principal_key",
            "document_id",
            "prepared_revision_id",
            "created_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_turn_outcomes",
        conflict_key: "thread_id,assistant_message_id",
        parents: &[
            "authored_turn_preparations",
            "authored_revisions",
            "agent_thread_messages",
        ],
        columns: &[
            "thread_id",
            "assistant_message_id",
            "owner_user_id",
            "principal_key",
            "document_id",
            "prepared_revision_id",
            "status",
            "result_revision_id",
            "conflicts_json",
            "created_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "agent_thread_deletions",
        conflict_key: "thread_id",
        parents: &["authored_documents", "agent_threads"],
        columns: &[
            "thread_id",
            "owner_user_id",
            "principal_key",
            "document_id",
            "deleted_at",
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
