# Auto-Sync Architecture

Design document for automatic background synchronization between local SQLite and Supabase cloud.

## Overview

Luma implements invisible, automatic sync of all user data to the cloud. This serves two purposes:
1. Future ML training on user-generated content (patterns, scores, harmonic analysis)
2. Cross-device data availability (future feature)

Sync is mandatory and not user-configurable. Local data always takes precedence over cloud data (no conflict resolution).

## Two-Worker Architecture

Sync is handled by two independent background workers with different characteristics:

### Tabular Sync Worker
- **Purpose**: Sync metadata rows to Supabase PostgreSQL tables
- **Interval**: Every 60 seconds
- **Payload size**: Small (KB per batch)
- **Behavior**: All-or-nothing batch sync per cycle
- **Tables**: venues, fixtures, categories, patterns, implementations, venue_overrides, tracks, track_beats, track_roots, track_waveforms, track_stems, scores, track_scores

### Storage Sync Worker
- **Purpose**: Upload binary files to Supabase Storage
- **Interval**: Continuous (processes queue sequentially)
- **Payload size**: Large (MB-GB per file)
- **Behavior**: One upload at a time, progress tracking, resumable
- **Files**: Track audio, artwork thumbnails, compressed analysis packages

## Change Detection

### Dirty Flag Approach

Each syncable table includes a `needs_sync` boolean column:

```
needs_sync BOOLEAN DEFAULT 1
```

- Set to `1` (true) on INSERT and UPDATE
- Set to `0` (false) after successful cloud sync
- Query for dirty records: `WHERE needs_sync = 1`

SQLite triggers automatically set `needs_sync = 1` on any row modification.

### Why Dirty Flags Over Timestamps

- Simpler queries (boolean vs timestamp comparison)
- No clock synchronization concerns
- Clear binary state: synced or not synced
- Triggers can atomically set the flag

## Tabular Sync Worker

### Lifecycle

1. Spawned during Tauri app `setup()` as a detached async task
2. Runs continuously until app termination
3. Persists no state between cycles (queries fresh each time)

### Sync Cycle

Each 60-second cycle:

1. **Authentication check**: Skip cycle if no valid access token
2. **Connectivity check**: Skip cycle if Supabase unreachable
3. **Query dirty records**: For each table, `SELECT * WHERE needs_sync = 1`
4. **Sync in dependency order**: Parents before children (see ordering below)
5. **Update sync status**: Set `needs_sync = 0` for successfully synced rows
6. **Sleep**: Wait 60 seconds before next cycle

### Dependency Order

Foreign key constraints require parent records exist before children:

**Tier 1 - No dependencies:**
- venues
- categories (pattern_categories)
- tracks

**Tier 2 - Single parent:**
- fixtures → venues
- patterns → categories
- scores → tracks
- track_beats → tracks
- track_roots → tracks
- track_waveforms → tracks
- track_stems → tracks

**Tier 3 - Multiple parents:**
- implementations → patterns
- track_scores → scores, patterns

**Tier 4 - Complex:**
- venue_implementation_overrides → venues, patterns, implementations

### Error Handling

- Individual row failures do not abort the cycle
- Failed rows retain `needs_sync = 1` and retry next cycle
- Errors are logged but not surfaced to user
- Network failures trigger early cycle termination

## Storage Sync Worker

### Three Uploads Per Track

Each imported track results in three separate storage uploads:

#### 1. Track Audio File
- **Source**: Original audio file (mp3, wav, flac, etc.)
- **Destination**: `tracks/{uid}/{track_hash}.{ext}`
- **Size**: 5-50 MB typical
- **Timing**: Queued immediately after track import

#### 2. Artwork Thumbnail
- **Source**: Extracted album art, resized to thumbnail dimensions
- **Destination**: `artwork/{uid}/{track_hash}_thumb.jpg`
- **Size**: 10-100 KB typical
- **Timing**: Queued after artwork extraction during import

#### 3. Analysis Package (Compressed)
- **Source**: Multiple files bundled and compressed
- **Destination**: `packages/{uid}/{track_hash}_analysis.tar.zst`
- **Size**: 50-200 MB typical (compression reduces significantly)
- **Timing**: Queued after all analysis workers complete

**Analysis Package Contents:**
| File | Description | Source |
|------|-------------|--------|
| `artwork.{ext}` | Full-resolution album art | `tracks.album_art_path` |
| `waveform.json` | Complete waveform data (full_samples, bands, colors) | `track_waveforms` table |
| `logits.bin` | Harmonic analysis neural network outputs | `track_roots.logits_path` |
| `stems/bass.wav` | Bass stem audio | `track_stems` |
| `stems/drums.wav` | Drums stem audio | `track_stems` |
| `stems/vocals.wav` | Vocals stem audio | `track_stems` |
| `stems/other.wav` | Other stem audio | `track_stems` |

The package is created using tar with zstd compression for optimal size/speed tradeoff.

### Storage Queue

Persistent queue in SQLite tracks pending uploads:

**Table: `storage_sync_queue`**

| Column | Type | Description |
|--------|------|-------------|
| id | INTEGER PK | Auto-increment ID |
| upload_type | TEXT | 'track_audio', 'artwork_thumb', 'analysis_package' |
| track_id | INTEGER | FK to tracks table |
| local_path | TEXT | Path to local file or package |
| storage_path | TEXT | Destination path in Supabase Storage (set after upload) |
| file_size | INTEGER | Total bytes to upload |
| bytes_uploaded | INTEGER | Progress for resumable uploads |
| status | TEXT | 'pending', 'uploading', 'completed', 'failed' |
| attempts | INTEGER | Retry counter |
| last_error | TEXT | Most recent error message |
| priority | INTEGER | Lower = higher priority (for ordering) |
| created_at | DATETIME | Queue insertion time |
| completed_at | DATETIME | Successful upload time |

### Queue Population

Upload jobs are inserted into the queue at specific points:

1. **Track import completes** → Queue `track_audio` job
2. **Artwork extraction completes** → Queue `artwork_thumb` job
3. **All analysis workers complete** → Queue `analysis_package` job

The analysis package job should only be queued when ALL of the following exist:
- `track_stems` (4 rows for the track)
- `track_roots` with `logits_path` populated
- `track_waveforms` with full data populated

A coordination mechanism (counter or completion flags) tracks when all workers finish.

### Worker Behavior

The storage worker runs continuously:

1. **Check preconditions**: Authenticated? Online? Skip iteration if not.
2. **Fetch next job**: `SELECT * FROM storage_sync_queue WHERE status IN ('pending', 'uploading') ORDER BY priority, created_at LIMIT 1`
3. **Resume or start**: If `bytes_uploaded > 0`, attempt resumable upload from that offset
4. **Upload with progress**: Stream file to Supabase Storage, updating `bytes_uploaded` periodically
5. **On success**: Set `status = 'completed'`, record `storage_path`, update entity's storage path column
6. **On failure**: Increment `attempts`, record `last_error`, set `status = 'failed'` if attempts > max
7. **Throttle**: Brief delay between uploads to avoid saturating network
8. **Loop**: Return to step 1

### Package Creation

Before uploading an analysis package, it must be assembled:

1. Create temporary directory
2. Copy full artwork to `artwork.{ext}`
3. Export full waveform data to `waveform.json`
4. Copy logits file to `logits.bin`
5. Copy all 4 stems to `stems/` subdirectory
6. Create tar archive with zstd compression
7. Queue the resulting `.tar.zst` file for upload
8. Clean up temporary directory after successful upload

### Priority System

Lower priority numbers are processed first:

| Priority | Upload Type | Rationale |
|----------|-------------|-----------|
| 1 | artwork_thumb | Small, quick win, visible to user |
| 2 | track_audio | Core data, moderate size |
| 3 | analysis_package | Large, can wait |

Within same priority, FIFO by `created_at`.

### Resumable Uploads

For large files (analysis packages), implement resumable uploads:

- Supabase Storage supports the TUS protocol for resumable uploads
- Track `bytes_uploaded` in queue table
- On app restart, check for `status = 'uploading'` jobs and resume
- If resume fails, restart from beginning

Initial implementation may skip TUS and restart failed uploads from scratch. Add resumability as optimization later.

## Offline Handling

### Detection

Simple connectivity check before each sync cycle:

- Attempt HEAD request to Supabase endpoint
- Timeout after 5 seconds
- If unreachable, skip cycle entirely

### Behavior When Offline

- Tabular sync: Skip cycle, retry in 60 seconds
- Storage sync: Pause current upload, retry when online
- Queue continues to accumulate jobs
- No data loss - dirty flags and queue persist in SQLite

### Recovery When Online

- Next tabular sync cycle processes all accumulated dirty records
- Storage worker resumes processing queue
- No special "catch up" logic needed - normal operation handles backlog

## App Lifecycle Events

### Startup

1. Spawn tabular sync worker
2. Spawn storage sync worker
3. Both workers begin their loops immediately

### Shutdown

**Sync-before-exit** ensures no data loss:

1. User initiates quit (Cmd+Q, close window, etc.)
2. Intercept quit event
3. Run one final tabular sync cycle (blocking)
4. Cancel in-progress storage upload gracefully
5. Proceed with app termination

Storage uploads resume on next app launch from the queue.

### Background/Foreground (Future)

If app is backgrounded (minimized, hidden):
- Workers continue running normally
- May reduce sync frequency to conserve resources
- Storage uploads may pause to reduce battery/bandwidth

## Database Schema Additions

### Migrations Required

Add to all syncable tables:
```sql
ALTER TABLE {table} ADD COLUMN needs_sync BOOLEAN DEFAULT 1;
```

Add triggers for automatic dirty flag:
```sql
CREATE TRIGGER {table}_set_needs_sync_insert
AFTER INSERT ON {table}
BEGIN
    UPDATE {table} SET needs_sync = 1 WHERE id = NEW.id;
END;

CREATE TRIGGER {table}_set_needs_sync_update
AFTER UPDATE ON {table}
WHEN OLD.needs_sync = 0
BEGIN
    UPDATE {table} SET needs_sync = 1 WHERE id = NEW.id;
END;
```

Create storage queue table (see schema above).

## Module Structure

```
src/
├── services/
│   ├── cloud_sync.rs          # Existing - upsert logic for each entity
│   ├── tabular_sync_worker.rs # NEW - 60s interval worker
│   ├── storage_sync_worker.rs # NEW - continuous upload worker
│   └── package_builder.rs     # NEW - analysis package creation
├── database/
│   └── local/
│       └── storage_queue.rs   # NEW - queue CRUD operations
```

## Configuration Constants

| Constant | Value | Description |
|----------|-------|-------------|
| TABULAR_SYNC_INTERVAL | 60s | Time between tabular sync cycles |
| STORAGE_THROTTLE_DELAY | 1s | Delay between storage uploads |
| CONNECTIVITY_TIMEOUT | 5s | Timeout for online check |
| MAX_UPLOAD_ATTEMPTS | 5 | Retries before marking failed |
| PACKAGE_COMPRESSION_LEVEL | 3 | Zstd compression level (1-22) |

## Security Considerations

- Access token validated before each sync operation
- Supabase RLS policies enforce user can only write own data
- Storage paths include `{uid}/` prefix for isolation
- Tokens refreshed automatically via existing auth flow

## Future Considerations

### Bandwidth Throttling
Add user-configurable upload speed limit to avoid saturating connection.

### Sync Status UI
Expose sync state to frontend: last sync time, pending uploads, progress.

### Selective Sync
Allow power users to exclude certain tracks from sync (not planned for MVP).

### Download/Restore
Pull data from cloud to new device. Requires conflict resolution strategy.

### Compression Tuning
Experiment with compression levels and formats for optimal size/speed.
