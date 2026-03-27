/// Static metadata for one syncable table.
///
/// All pull queries rely on Supabase RLS for visibility scoping —
/// no client-side filtering needed.
#[derive(Debug)]
pub struct TableMeta {
    /// Supabase table name (matches the local SQLite table name).
    pub name: &'static str,
    /// Column(s) for ON CONFLICT during upsert. Usually `"id"` but can be
    /// composite like `"track_id,stem_name"` or `"venue_id,pattern_id"`.
    pub conflict_key: &'static str,
    /// FK dependency tier (0 = no deps, 3 = most deps). Tables are pulled
    /// and pushed in ascending tier order.
    pub tier: u8,
    /// Column names for the local INSERT. Order matters — binds in this order.
    pub columns: &'static [&'static str],
    /// Columns that exist locally but NOT on the remote. Excluded from the
    /// Supabase SELECT query. The pull code must inject values for these.
    pub local_only: &'static [&'static str],
}

// ============================================================================
// The registry — one entry per syncable table, ordered by tier then name.
// ============================================================================

pub static TABLES: &[TableMeta] = &[
    // Tier 0: no FK dependencies
    TableMeta {
        name: "venues",
        conflict_key: "id",
        tier: 0,

        // role is local-only but venues are synced through discovery, not pull_table.
        // Delta pull only gets owned venues (OwnedByUid), so role is always 'owner'.
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
        tier: 0,

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
            "created_at",
            "updated_at",
        ],
        local_only: &["file_path"],
    },
    TableMeta {
        name: "pattern_categories",
        conflict_key: "id",
        tier: 0,

        columns: &["id", "uid", "name", "created_at", "updated_at"],
        local_only: &[],
    },
    // Tier 1: single parent FK
    TableMeta {
        name: "fixtures",
        conflict_key: "id",
        tier: 1,

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
        tier: 1,

        columns: &[
            "id",
            "uid",
            "name",
            "description",
            "category_id",
            "is_published",
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
        tier: 1,

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
    // Tier 2: multiple parent FKs or single parent from tier 1
    TableMeta {
        name: "implementations",
        conflict_key: "id",
        tier: 2,

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
        tier: 2,

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
        tier: 2,

        columns: &[
            "track_id",
            "uid",
            "bpm",
            "beats_json",
            "downbeats_json",
            "downbeat_offset",
            "beats_per_bar",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    TableMeta {
        name: "track_roots",
        conflict_key: "track_id",
        tier: 2,

        columns: &[
            "track_id",
            "uid",
            "sections_json",
            "logits_storage_path",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
    // track_waveforms has a significant schema mismatch between local and remote
    // (blob columns vs array columns, decoded_duration vs duration_seconds).
    // Excluded from generic sync — handled by the old Syncable payload mapping.
    // TODO: add column mapping support to the sync engine
    // TableMeta {
    //     name: "track_waveforms",
    //     ...
    // },
    TableMeta {
        name: "track_stems",
        conflict_key: "track_id,stem_name",
        tier: 2,

        columns: &[
            "track_id",
            "uid",
            "stem_name",
            "file_path",
            "storage_path",
            "created_at",
            "updated_at",
        ],
        // file_path is local-only — pull.rs injects empty string default
        local_only: &["file_path"],
    },
    TableMeta {
        name: "fixture_group_members",
        conflict_key: "fixture_id,group_id",
        tier: 2,

        columns: &["fixture_id", "group_id", "display_order", "updated_at"],
        local_only: &[],
    },
    // Tier 3: complex dependencies
    TableMeta {
        name: "track_scores",
        conflict_key: "id",
        tier: 3,

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
    TableMeta {
        name: "venue_implementation_overrides",
        conflict_key: "venue_id,pattern_id",
        tier: 3,

        columns: &[
            "venue_id",
            "pattern_id",
            "implementation_id",
            "uid",
            "created_at",
            "updated_at",
        ],
        local_only: &[],
    },
];

/// Return tables grouped by tier, in ascending tier order.
pub fn tables_by_tier() -> Vec<(u8, Vec<&'static TableMeta>)> {
    let max_tier = TABLES.iter().map(|t| t.tier).max().unwrap_or(0);
    (0..=max_tier)
        .map(|tier| {
            let tables: Vec<&TableMeta> = TABLES.iter().filter(|t| t.tier == tier).collect();
            (tier, tables)
        })
        .filter(|(_, tables)| !tables.is_empty())
        .collect()
}
