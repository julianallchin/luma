/// Static metadata for one syncable table.
///
/// All pull queries rely on Supabase RLS for visibility scoping —
/// no client-side filtering needed.
#[derive(Debug)]
pub struct TableMeta {
    pub name: &'static str,
    /// Column(s) for ON CONFLICT during upsert, and used to derive PK
    /// columns for WHERE clauses and record ID encoding.
    pub conflict_key: &'static str,
    /// FK dependency tier (0 = no deps, 3 = most deps).
    pub tier: u8,
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
                "SELECT {pk_select} FROM {} WHERE uid = ? AND (synced_at IS NULL OR updated_at > synced_at)",
                self.name
            )
        } else {
            format!(
                "SELECT {pk_select} FROM {} WHERE synced_at IS NULL OR updated_at > synced_at",
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
    // Tier 0
    TableMeta {
        name: "venues",
        conflict_key: "id",
        tier: 0,
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
            "album_art_storage_path",
            "created_at",
            "updated_at",
        ],
        local_only: &["file_path", "album_art_path"],
    },
    // pattern_categories excluded: seeded by migration, same on every device
    // Tier 1
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
    // Tier 2
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
    // track_waveforms excluded: local/remote schema mismatch (blob vs array columns)
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
        local_only: &["file_path"],
    },
    TableMeta {
        name: "fixture_group_members",
        conflict_key: "fixture_id,group_id",
        tier: 2,
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
    // Tier 3
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
];

/// Tables grouped by tier, ascending. Computed once.
pub fn tables_by_tier() -> Vec<(u8, Vec<&'static TableMeta>)> {
    let max_tier = TABLES.iter().map(|t| t.tier).max().unwrap_or(0);
    (0..=max_tier)
        .filter_map(|tier| {
            let tables: Vec<&TableMeta> = TABLES.iter().filter(|t| t.tier == tier).collect();
            if tables.is_empty() {
                None
            } else {
                Some((tier, tables))
            }
        })
        .collect()
}
