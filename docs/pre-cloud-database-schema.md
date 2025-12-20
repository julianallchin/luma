## Database Table Structures

Source: current SQLx migrations in `src-tauri/migrations/**` plus the checked-in project databases `projects/med.luma` and `projects/tetra.luma` (both empty except for schema). All column defaults and constraints are listed explicitly.

### App Database (`luma.db`)

- `patterns`
  - `id` INTEGER PRIMARY KEY AUTOINCREMENT
  - `name` TEXT NOT NULL
  - `description` TEXT
  - `category_id` INTEGER REFERENCES `pattern_categories(id)` ON DELETE SET NULL
  - `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - Indexes: `patterns_category_idx` on `category_id`
  - Triggers: `patterns_updated_at` sets `updated_at` to CURRENT_TIMESTAMP on updates when unchanged

- `pattern_categories`
  - `id` INTEGER PRIMARY KEY AUTOINCREMENT
  - `name` TEXT NOT NULL UNIQUE
  - `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - Triggers: `pattern_categories_updated_at` auto-updates `updated_at`

- `tracks`
  - `id` INTEGER PRIMARY KEY AUTOINCREMENT
  - `track_hash` TEXT NOT NULL UNIQUE
  - `title` TEXT
  - `artist` TEXT
  - `album` TEXT
  - `track_number` INTEGER
  - `disc_number` INTEGER
  - `duration_seconds` REAL
  - `file_path` TEXT NOT NULL
  - `album_art_path` TEXT
  - `album_art_mime` TEXT
  - `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - Triggers: `tracks_updated_at` auto-updates `updated_at`

- `track_beats`
  - `track_id` INTEGER PRIMARY KEY REFERENCES `tracks(id)` ON DELETE CASCADE
  - `beats_json` TEXT NOT NULL
  - `downbeats_json` TEXT NOT NULL
  - `bpm` REAL
  - `downbeat_offset` REAL
  - `beats_per_bar` INTEGER
  - `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - Triggers: `track_beats_updated_at` auto-updates `updated_at`

- `track_roots`
  - `track_id` INTEGER PRIMARY KEY REFERENCES `tracks(id)` ON DELETE CASCADE
  - `sections_json` TEXT NOT NULL
  - `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `logits_path` TEXT
  - Triggers: `track_roots_updated_at` auto-updates `updated_at`

- `track_stems`
  - `track_id` INTEGER NOT NULL REFERENCES `tracks(id)` ON DELETE CASCADE
  - `stem_name` TEXT NOT NULL
  - `file_path` TEXT NOT NULL
  - `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - Primary key: (`track_id`, `stem_name`)
  - Triggers: `track_stems_updated_at` auto-updates `updated_at`

- `track_waveforms`
  - `track_id` INTEGER PRIMARY KEY REFERENCES `tracks(id)` ON DELETE CASCADE
  - `preview_samples_json` TEXT NOT NULL
  - `full_samples_json` TEXT
  - `colors_json` TEXT
  - `sample_rate` INTEGER NOT NULL
  - `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `preview_colors_json` TEXT
  - `bands_json` TEXT
  - `preview_bands_json` TEXT
  - Triggers: `track_waveforms_updated_at` auto-updates `updated_at`

- `track_annotations`
  - `id` INTEGER PRIMARY KEY AUTOINCREMENT
  - `track_id` INTEGER NOT NULL REFERENCES `tracks(id)` ON DELETE CASCADE
  - `pattern_id` INTEGER NOT NULL REFERENCES `patterns(id)` ON DELETE CASCADE
  - `start_time` REAL NOT NULL
  - `end_time` REAL NOT NULL
  - `z_index` INTEGER NOT NULL DEFAULT 0
  - `args_json` TEXT NOT NULL DEFAULT '{}'
  - `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `blend_mode` TEXT NOT NULL DEFAULT 'replace'
  - Indexes: `track_annotations_track_idx` on `track_id`
  - Triggers: `track_annotations_updated_at` auto-updates `updated_at`

- `recent_projects`
  - `path` TEXT PRIMARY KEY
  - `name` TEXT NOT NULL
  - `last_opened` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP

- `settings`
  - `key` TEXT PRIMARY KEY NOT NULL
  - `value` TEXT NOT NULL
  - Seed rows inserted by migration:
    - (`artnet_enabled`, `false`)
    - (`artnet_interface`, `0.0.0.0`)
    - (`artnet_broadcast`, `true`)
    - (`artnet_unicast_ip`, ``)
    - (`artnet_net`, `0`)
    - (`artnet_subnet`, `0`)

- `_sqlx_migrations`
  - `version` BIGINT PRIMARY KEY
  - `description` TEXT NOT NULL
  - `installed_on` TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `success` BOOLEAN NOT NULL
  - `checksum` BLOB NOT NULL
  - `execution_time` BIGINT NOT NULL
  - (This table is created automatically by SQLx when migrations are applied; the structure matches the one visible in the `.luma` project files.)

### Venue Project Databases (`*.luma`)

Each project file is a SQLite database. The schema in `projects/med.luma` and `projects/tetra.luma` matches the project migrations.

- `_sqlx_migrations`
  - `version` BIGINT PRIMARY KEY
  - `description` TEXT NOT NULL
  - `installed_on` TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `success` BOOLEAN NOT NULL
  - `checksum` BLOB NOT NULL
  - `execution_time` BIGINT NOT NULL

- `implementations`
  - `pattern_id` INTEGER PRIMARY KEY
  - `graph_json` TEXT NOT NULL DEFAULT '{"nodes":[],"edges":[]}'
  - `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - Triggers: `implementations_updated_at` auto-updates `updated_at`

- `fixtures`
  - `id` TEXT PRIMARY KEY
  - `universe` INTEGER NOT NULL DEFAULT 1
  - `address` INTEGER NOT NULL
  - `num_channels` INTEGER NOT NULL
  - `manufacturer` TEXT NOT NULL
  - `model` TEXT NOT NULL
  - `mode_name` TEXT NOT NULL
  - `fixture_path` TEXT NOT NULL
  - `label` TEXT
  - `pos_x` REAL DEFAULT 0.0
  - `pos_y` REAL DEFAULT 0.0
  - `pos_z` REAL DEFAULT 0.0
  - `rot_x` REAL DEFAULT 0.0
  - `rot_y` REAL DEFAULT 0.0
  - `rot_z` REAL DEFAULT 0.0
  - `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  - Indexes: `idx_fixtures_universe` on `universe`
  - Triggers: `fixtures_updated_at` auto-updates `updated_at`
