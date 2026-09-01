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
    /// Mutable state carrying the dirtiness triple (`updated_at`, `version`,
    /// `synced_at`). Push sends the row when its marker is behind its content
    /// and stamps the marker on delivery.
    DirtyUpsert,
    /// An append-only row. It has no second state to be in, so `synced_at IS
    /// NULL` is the whole question, and the remote accepts an exact replay.
    ExplicitImmutable,
    /// Mutable state with a delivery marker but no `version` to guard it, so
    /// its receipt is guarded on `updated_at` instead.
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

/// Whether the local table carries a `synced_at` delivery marker.
///
/// Every table push can deliver has one; the server-authoritative projections
/// do not, because a client never delivers them. Pull stamps it so a row that
/// arrived from the server is not immediately pushed back.
pub fn has_delivery_marker(name: &str) -> bool {
    !matches!(push_policy(name), PushPolicy::ServerAuthority)
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

/// One edge of the registry's dependency graph.
///
/// Every edge orders the topological sort. An edge that is also a real foreign
/// key names the local column holding the parent's primary key, which is what
/// lets push ask "is this row's parent reachable on the server yet?" — the
/// question whose absence produced FK 403s and unpushable orphans.
///
/// `via: None` is an ordering edge only: `authored_document_heads` must be
/// pulled after `authored_revision_files`, but holds no reference to one.
#[derive(Clone, Copy, Debug)]
pub struct Parent {
    pub table: &'static str,
    pub via: Option<&'static str>,
}

impl Parent {
    /// A foreign key: `column` holds the parent's single-column primary key.
    /// A NULL in it means "no parent", not "missing parent".
    pub const fn fk(table: &'static str, column: &'static str) -> Self {
        Self {
            table,
            via: Some(column),
        }
    }

    /// An ordering edge with no reference to follow.
    pub const fn order(table: &'static str) -> Self {
        Self { table, via: None }
    }
}

#[derive(Debug)]
pub struct TableMeta {
    pub name: &'static str,
    /// Column(s) for ON CONFLICT during upsert, and used to derive PK
    /// columns for WHERE clauses and record ID encoding.
    pub conflict_key: &'static str,
    /// Tables this row depends on. Must reference names present in [`TABLES`];
    /// cycles or unknown names panic at startup. See [`Parent`] for the
    /// difference between an ordering edge and a real foreign key.
    pub parents: &'static [Parent],
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

    /// The SQL expression for this row's `record_id`, as the tombstone ledger
    /// and the retry table encode it.
    pub fn record_id_expr(&self, alias: &str) -> String {
        self.pk_columns()
            .iter()
            .map(|column| format!("{alias}.{column}"))
            .collect::<Vec<_>>()
            .join(" || char(31) || ")
    }

    /// `version` where the table has one, `NULL` otherwise. Only `DirtyUpsert`
    /// tables carry the dirtiness triple; an immutable row has no version
    /// because it has no second state to be in.
    fn version_expr(&self, alias: &str) -> String {
        if push_policy(self.name) == PushPolicy::DirtyUpsert {
            format!("{alias}.version")
        } else {
            "NULL".to_owned()
        }
    }

    /// A second witness that the row has not moved, for the one mutable table
    /// with no `version` column.
    fn stamp_expr(&self, alias: &str) -> String {
        if push_policy(self.name) == PushPolicy::ExplicitUpsert {
            format!("{alias}.updated_at")
        } else {
            "NULL".to_owned()
        }
    }

    /// Restrict a scan to rows this principal owns.
    fn principal_predicate(&self, alias: &str, uid_param: &str) -> String {
        if self.columns.contains(&"uid") {
            format!("{alias}.uid = {uid_param}")
        } else if self.columns.contains(&"principal_key") {
            format!("{alias}.principal_key = 'signed-in:' || {uid_param}")
        } else if self.columns.contains(&"owner_user_id") {
            format!("{alias}.owner_user_id = {uid_param}")
        } else {
            "1 = 1".to_owned()
        }
    }

    /// "This row still owes the server something", read from the row itself.
    ///
    /// `synced_at IS NULL` is what the dirtiness triggers write. The second
    /// clause is not redundant: it catches a trigger that bumped `updated_at`
    /// without clearing `synced_at` (`track_genres_updated_at` has always done
    /// exactly that), and it is an inequality rather than `>` because the two
    /// sides are the same value copied by the delivery receipt — a `>` would be
    /// comparing `CURRENT_TIMESTAMP`'s `'YYYY-MM-DD HH:MM:SS'` against the
    /// triggers' `'…THH:MM:SSZ'`, where `'T' > ' '` makes every row look dirty.
    fn dirty_predicate(&self, alias: &str) -> String {
        match push_policy(self.name) {
            PushPolicy::DirtyUpsert | PushPolicy::ExplicitUpsert => {
                format!("({alias}.synced_at IS NULL OR {alias}.synced_at <> {alias}.updated_at)")
            }
            // An immutable row is delivered once and never changes, so its
            // marker is the whole question.
            PushPolicy::ExplicitImmutable => format!("{alias}.synced_at IS NULL"),
            // Proposals and archives are dirty until the RPC assigns their
            // sequence; heads and transcript heads are never pushed at all.
            PushPolicy::ServerAuthority => match self.name {
                "authored_head_proposals" => format!("{alias}.server_proposal_seq IS NULL"),
                "authored_document_archives" => format!("{alias}.server_archive_seq IS NULL"),
                _ => "0 = 1".to_owned(),
            },
        }
    }

    /// One clause per foreign key: the parent must be there, and must itself
    /// have reached the server. A row whose parent has not landed would be
    /// refused with a 403 or a 409 and retried forever; skipping it costs
    /// nothing, because the next scan re-derives it for free.
    fn reachability_predicates(&self, alias: &str) -> Vec<String> {
        self.parents
            .iter()
            .filter_map(|parent| {
                let column = parent.via?;
                let table = get_table(parent.table)?;
                let key = table.pk_columns();
                debug_assert_eq!(
                    key.len(),
                    1,
                    "{}.{column} names {} as a foreign key, but its key is composite",
                    self.name,
                    parent.table
                );
                let delivered = match push_policy(parent.table) {
                    PushPolicy::ServerAuthority => String::new(),
                    _ => " AND parent.synced_at IS NOT NULL".to_owned(),
                };
                Some(format!(
                    "({alias}.{column} IS NULL OR EXISTS (SELECT 1 FROM {} AS parent \
                     WHERE parent.{} = {alias}.{column}{delivered}))",
                    parent.table, key[0],
                ))
            })
            .collect()
    }

    /// Every row of this table that push owes the server right now.
    ///
    /// Binds, in order: the principal key (`signed-in:<uid>`) for the retry
    /// join, then the uid. Selects the primary-key columns, the row's `version`
    /// (or NULL), then its `updated_at` stamp (or NULL).
    pub fn dirty_scan_sql(&self, limit: u32) -> String {
        let selected: Vec<String> = self
            .pk_columns()
            .iter()
            .map(|column| format!("subject.{column}"))
            .collect();
        let version = self.version_expr("subject");
        let mut conditions = vec![
            self.principal_predicate("subject", "?2"),
            self.dirty_predicate("subject"),
            super::push_state::ready_predicate(&version),
        ];
        conditions.extend(self.reachability_predicates("subject"));
        format!(
            "SELECT {}, {}, {} FROM {} AS subject
             LEFT JOIN sync_push_failures AS failure
                    ON failure.principal_key = ?1
                   AND failure.table_name = '{}'
                   AND failure.record_id = {}
                   AND failure.subject = 'row'
             WHERE {}
             LIMIT {limit}",
            selected.join(", "),
            version,
            self.stamp_expr("subject"),
            self.name,
            self.name,
            self.record_id_expr("subject"),
            conditions.join("\n               AND "),
        )
    }

    /// How many rows this principal still owes the server.
    ///
    /// The scan's dirty predicate without reachability or backoff — "is there
    /// outstanding work" is a different question from "what should go next" —
    /// but *with* the permanent verdict, because a row push has given up on is
    /// not outstanding work. It is abandoned, and sign-out reports it rather
    /// than waiting for it.
    ///
    /// `None` for a table with no principal or no delivery marker: there is
    /// nothing this principal could owe. Binds the uid once.
    pub fn undelivered_count_sql(&self) -> Option<String> {
        if !self.has_principal() || !has_delivery_marker(self.name) {
            return None;
        }
        Some(format!(
            "SELECT COUNT(*) FROM {} AS subject
             LEFT JOIN sync_push_failures AS failure
                    ON failure.principal_key = 'signed-in:' || ?1
                   AND failure.table_name = '{}'
                   AND failure.record_id = {}
                   AND failure.subject = 'row'
             WHERE {} AND {} AND COALESCE(failure.permanent, 0) = 0",
            self.name,
            self.name,
            self.record_id_expr("subject"),
            self.principal_predicate("subject", "?1"),
            self.dirty_predicate("subject"),
        ))
    }

    /// Write the delivery receipt onto the row.
    ///
    /// `DirtyUpsert` bumps `version` so the table's own `*_updated_at` trigger —
    /// which fires `WHEN OLD.version = NEW.version` — does not immediately mark
    /// the row dirty again. The other policies have no version to bump and run
    /// inside a sync-owned write, where their triggers stand down.
    pub fn mark_delivered_sql(&self) -> String {
        match push_policy(self.name) {
            PushPolicy::DirtyUpsert => format!(
                "UPDATE {} SET synced_at = updated_at, version = version + 1 WHERE {}",
                self.name,
                self.pk_where()
            ),
            PushPolicy::ExplicitUpsert => format!(
                "UPDATE {} SET synced_at = updated_at WHERE {}",
                self.name,
                self.pk_where()
            ),
            _ => format!(
                "UPDATE {} SET synced_at = CURRENT_TIMESTAMP WHERE {}",
                self.name,
                self.pk_where()
            ),
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

    /// Decode a record id into one value per primary-key column, or `None` if
    /// it does not carry exactly one.
    ///
    /// The arity is checked here rather than at the four call sites because
    /// every one of them turns the values into a `WHERE` clause or a PostgREST
    /// filter: a short list would silently *widen* it to every row sharing the
    /// prefix, which for a delete means every parameter of a node instead of
    /// one.
    pub fn decode_record_id<'a>(&self, record_id: &'a str) -> Option<Vec<&'a str>> {
        let columns = self.pk_columns().len();
        if columns == 1 {
            return Some(vec![record_id]);
        }
        let values: Vec<&str> = record_id.split(RECORD_ID_SEPARATOR).collect();
        (values.len() == columns).then_some(values)
    }
}

/// The separator between primary-key values inside a composite record id.
///
/// A control character rather than something typeable, because decoding is a
/// split and any value that can contain the separator makes that split
/// ambiguous. `':'` used to serve, on the unstated assumption that only the
/// *last* value could contain one — which `venue_node_params` breaks, since a
/// venue's root node is named `"<venue_id>:venue"` by `venue_graph::migrate`
/// and cannot be renamed (the id is immutable by admission trigger). Nothing
/// that reaches a primary key here — an id, a socket name, a parameter key, a
/// path — contains a unit separator.
pub const RECORD_ID_SEPARATOR: char = '\u{1f}';

/// One row's identity as the tombstone ledger and the retry table spell it,
/// from its primary-key values in `pk_columns` order.
///
/// The inverse of [`TableMeta::decode_record_id`]; the delete guards on
/// composite tables build the same string with `char(31)`.
pub fn record_id<'a>(keys: impl IntoIterator<Item = &'a str>) -> String {
    keys.into_iter()
        .collect::<Vec<_>>()
        .join(&RECORD_ID_SEPARATOR.to_string())
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
    // `address_pinned` rides along with `address` and `universe`: the pin is a
    // property of the address, so a push that moves one moves the other. It is
    // an integer, not a bool, because `read_record_as_json` sends SQLite
    // INTEGER as a JSON number and PostgREST refuses `0` for a boolean.
    TableMeta {
        name: "fixtures",
        conflict_key: "id",
        parents: &[Parent::fk("venues", "venue_id")],
        columns: &[
            "id",
            "uid",
            "venue_id",
            "universe",
            "address",
            "address_pinned",
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
    // The venue graph. A node's placement is an `venue_edges` row keyed by the
    // child, its parameters are `venue_node_params` rows and its far-end checks
    // are `venue_constraints` rows — all three reach their venue through the
    // node, which is what the remote RLS checks and what gives them their
    // `uid`. Rows the three carry are meaningless without their node, so the
    // node is their only registered parent.
    TableMeta {
        name: "venue_nodes",
        conflict_key: "id",
        parents: &[Parent::fk("venues", "venue_id")],
        columns: &[
            "id",
            "uid",
            "venue_id",
            "kind",
            "catalog_ref",
            "label",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "venue_edges",
        conflict_key: "child_id",
        parents: &[
            Parent::fk("venue_nodes", "child_id"),
            Parent::fk("venue_nodes", "parent_id"),
        ],
        columns: &[
            "child_id",
            "uid",
            "parent_id",
            "my_socket",
            "their_socket",
            "roll",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "venue_node_params",
        conflict_key: "node_id,key",
        parents: &[Parent::fk("venue_nodes", "node_id")],
        columns: &["node_id", "uid", "key", "value", "created_at", "updated_at"],
        local_only: &[],
    },
    TableMeta {
        name: "venue_constraints",
        conflict_key: "node_id,my_socket",
        parents: &[
            Parent::fk("venue_nodes", "node_id"),
            Parent::fk("venue_nodes", "target_node"),
        ],
        columns: &[
            "node_id",
            "uid",
            "my_socket",
            "target_node",
            "target_socket",
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
        parents: &[Parent::fk("venues", "venue_id")],
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
        parents: &[Parent::fk("venues", "venue_id")],
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
        parents: &[
            Parent::fk("tracks", "track_id"),
            Parent::fk("venues", "venue_id"),
        ],
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
        parents: &[Parent::fk("tracks", "track_id")],
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
        parents: &[Parent::fk("tracks", "track_id")],
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
        parents: &[Parent::fk("tracks", "track_id")],
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
        parents: &[Parent::fk("tracks", "track_id")],
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
        parents: &[Parent::fk("tracks", "track_id")],
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
        parents: &[Parent::fk("tracks", "track_id")],
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
        parents: &[
            Parent::fk("fixtures", "fixture_id"),
            Parent::fk("fixture_groups", "group_id"),
        ],
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
        parents: &[
            Parent::fk("venues", "venue_id"),
            Parent::fk("patterns", "pattern_id"),
        ],
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
        parents: &[Parent::fk("venues", "venue_id")],
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
        parents: &[Parent::fk("authored_documents", "document_id")],
        columns: &[
            "revision_id",
            "document_id",
            "principal_key",
            "parent_count",
            "content_hash",
            "operation_kind",
            "operation_id",
            "message",
            "actor",
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
        parents: &[Parent::fk("authored_revisions", "revision_id")],
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
        parents: &[Parent::fk("authored_revisions", "revision_id")],
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
            Parent::order("authored_revisions"),
            Parent::order("authored_revision_files"),
            Parent::order("authored_revision_parents"),
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
            Parent::fk("authored_documents", "document_id"),
            Parent::order("authored_revisions"),
            Parent::order("authored_revision_files"),
            Parent::order("authored_revision_parents"),
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
            Parent::fk("authored_documents", "document_id"),
            Parent::order("authored_revisions"),
            Parent::order("authored_revision_files"),
            Parent::order("authored_revision_parents"),
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
            Parent::order("authored_head_proposals"),
            Parent::order("authored_revisions"),
            Parent::order("authored_revision_files"),
            Parent::order("authored_revision_parents"),
            Parent::order("authored_document_heads"),
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
            Parent::fk("authored_documents", "document_id"),
            Parent::order("authored_revisions"),
            Parent::order("authored_revision_files"),
            Parent::order("authored_revision_parents"),
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
    // `actor` is deliberately absent: the thread's session label is this
    // host's, restamped every turn, and what has to survive is the copy each
    // revision keeps. A column sync does not name is left alone on upsert.
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
            "parent_thread_id",
            "parent_call_id",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "agent_thread_messages",
        conflict_key: "id",
        parents: &[
            Parent::fk("agent_threads", "created_in_thread_id"),
            Parent::order("authored_turn_preparations"),
        ],
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
        parents: &[
            Parent::order("agent_threads"),
            Parent::order("agent_thread_messages"),
        ],
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
        parents: &[Parent::fk("agent_thread_messages", "first_message_id")],
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
            Parent::fk("agent_threads", "thread_id"),
            Parent::fk("authored_revisions", "prepared_revision_id"),
            Parent::order("authored_revision_files"),
            Parent::order("authored_revision_parents"),
        ],
        columns: &[
            "thread_id",
            "assistant_message_id",
            "owner_user_id",
            "principal_key",
            "document_id",
            "prepared_revision_id",
            "workspace_id",
            "created_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "authored_turn_outcomes",
        conflict_key: "thread_id,assistant_message_id",
        parents: &[
            Parent::order("authored_turn_preparations"),
            Parent::order("authored_revisions"),
            Parent::order("agent_thread_messages"),
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
        parents: &[
            Parent::fk("authored_documents", "document_id"),
            Parent::fk("agent_threads", "thread_id"),
        ],
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
/// via [`crate::topo::flat`] and memoised. Pull and push both scan in this
/// order; push reverses it for tombstones, via [`topo_position`].
pub fn tables_in_topo_order() -> &'static [&'static TableMeta] {
    static ORDER: OnceLock<Vec<&'static TableMeta>> = OnceLock::new();
    ORDER.get_or_init(|| {
        topo::flat(
            TABLES,
            |t| t.name,
            |t| t.parents.iter().map(|parent| parent.table).collect(),
        )
    })
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
                    pos[parent.table] < pos[t.name],
                    "{} (idx {}) precedes its parent {} (idx {})",
                    t.name,
                    pos[t.name],
                    parent.table,
                    pos[parent.table]
                );
            }
        }
    }

    /// A venue's root node is named `"<venue_id>:venue"`, so the first key
    /// column of a `venue_node_params` record id contains a colon. Round-trip
    /// it, and refuse an id that names too few columns rather than letting a
    /// caller build a `WHERE` clause that matches the whole node.
    #[test]
    fn a_composite_record_id_survives_a_colon_in_its_first_key() {
        let table = get_table("venue_node_params").expect("registered");
        let encoded = record_id(["v-1:venue", "span"]);
        assert_eq!(
            table.decode_record_id(&encoded).as_deref(),
            Some(&["v-1:venue", "span"][..])
        );
        assert_eq!(table.decode_record_id("v-1:venue:span"), None);
        let single = get_table("venues").expect("registered");
        assert_eq!(single.decode_record_id("v:1"), Some(vec!["v:1"]));
    }

    #[test]
    fn topo_position_is_consistent_with_order() {
        for (i, t) in tables_in_topo_order().iter().enumerate() {
            assert_eq!(topo_position(t.name), Some(i));
        }
        assert_eq!(topo_position("nonexistent"), None);
    }
}
