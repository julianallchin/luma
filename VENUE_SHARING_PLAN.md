# Venue Sharing — Implementation Plan

## Context

Multiple DJs need to annotate light shows for a shared venue, and a single perform laptop (the venue owner's) plays back all scores from all DJs. Today the app is single-user: one person creates a venue, annotates tracks, and performs. This plan adds multi-DJ collaboration scoped to a venue, with the venue owner as the single source of truth.

### Core Flow
1. **Owner** creates venue, patches fixtures, shares a join code
2. **DJs** enter the code → get venue config (fixtures, groups, universe) locally → annotate their tracks → sync scores to cloud
3. **Perform laptop** (owner's account) pulls all scores for the venue from all DJs + their patterns, implementations, audio, stems, beats, roots → plays back via the existing compositor

### Key Design Decisions
- **Single owner** — only the venue creator can edit fixtures/groups/universe. Members get read-only.
- **Scores live in the same local tables** — no separate "foreign" storage. The `uid` field distinguishes ownership.
- **Patterns from other DJs are hidden in the pattern browser** — filtered by `uid`, same as existing "mine" vs "community" filter.
- **Compositor is unchanged** — all data lands in local SQLite with valid file paths. Existing read paths work.
- **Audio/stems must be in the cloud** — compositor fails hard on missing audio. Cloud storage is a prerequisite for playback on the owner's machine.
- **Venue implementation overrides are dead code** — ignored entirely.

---

## Epic 0: Compress Stems Locally (Optional, No Dependencies)

Stems are currently stored as uncompressed WAV (~50MB each). Switching to FLAC cuts that ~50% with zero quality loss. This reduces both local disk usage and future upload bandwidth.

**Can be done in parallel with everything else. Not on the critical path.**

### Task 0.1: Change demucs output to FLAC
- **File:** `src-tauri/python/audio_preprocessor.py`
- **Change:** In the `soundfile.write()` call, output FLAC instead of WAV. Demucs writes stems via soundfile — just change the format parameter and file extension.
- **Output files become:** `drums.flac`, `bass.flac`, `vocals.flac`, `other.flac`

### Task 0.2: Update stem file extension in Rust
- **File:** `src-tauri/src/stem_worker.rs`
- **Change:** Update the expected stem file extensions from `.wav` to `.flac` when scanning demucs output directory.
- **File:** `src-tauri/src/services/tracks.rs` (in `ensure_track_stems_for_path` and `persist_track_stems`)
- **Change:** Update any hardcoded `.wav` references in stem path construction (e.g., `bass.wav` → `bass.flac`).

### Task 0.3: Verify decode pipeline handles FLAC
- `load_or_decode_audio()` in `src-tauri/src/audio/cache.rs` uses ffmpeg as primary decoder — FLAC is natively supported. Symphonia fallback also supports FLAC.
- **No code changes needed** — just verify with a manual test.
- The disk PCM cache (`tracks/cache/*.pcm`) is format-agnostic (stores decoded f32 samples).
- StemCache is in-memory decoded samples — format-agnostic.

### Task 0.4: Migration for existing stems
- Existing users have WAV stems on disk. Options:
  - (a) Leave old WAVs in place — they still decode fine. New stems will be FLAC.
  - (b) Add a one-time migration that re-encodes existing WAVs to FLAC (complex, probably not worth it).
- **Recommendation:** Option (a). Old stems work. New ones are FLAC. No migration needed.

---

## Epic 1: Supabase Storage for Audio & Stems (No Dependencies)

The compositor requires audio files and stems on the local filesystem. For the owner's perform laptop to play back DJ scores, those files must be uploaded to the cloud and downloadable. Currently `storage_path` fields exist on `tracks` and `track_stems` tables but are always NULL.

### Task 1.1: Create Supabase storage buckets
- **Where:** Supabase dashboard (not app code)
- **Buckets:**
  - `track-audio` — source audio files (MP3/AAC/FLAC, ~5-10MB each)
  - `track-stems` — stem files (FLAC or WAV, ~3-25MB each)
- **RLS policies:** Authenticated users can upload to their own path (`{uid}/{track_hash}/...`). Any authenticated user can download (needed for venue members to pull data). DELETE only by owner (`uid` match).
- **Path convention:**
  - Audio: `{uid}/{track_hash}/audio.{ext}`
  - Stems: `{uid}/{track_hash}/stems/{stem_name}.{ext}`

### Task 1.2: Add Supabase storage client to Rust backend
- **File:** `src-tauri/src/database/remote/common.rs`
- **Add methods to `SupabaseClient`:**
  - `upload_file(bucket: &str, path: &str, file_bytes: &[u8], content_type: &str, access_token: &str) -> Result<String, SyncError>` — POST to `/storage/v1/object/{bucket}/{path}`, returns storage path
  - `download_file(bucket: &str, path: &str, access_token: &str) -> Result<Vec<u8>, SyncError>` — GET from `/storage/v1/object/{bucket}/{path}`, returns file bytes
  - `delete_file(bucket: &str, path: &str, access_token: &str) -> Result<(), SyncError>`
- **Note:** Supabase storage API is separate from PostgREST. Uses same auth headers but different URL path (`/storage/v1/` instead of `/rest/v1/`).

### Task 1.3: Upload track audio on sync
- **File:** `src-tauri/src/services/cloud_sync.rs` — add to `sync_track()` or `sync_track_with_children()`
- **Flow:**
  1. After upserting track metadata to Supabase, check if `storage_path` is NULL
  2. Read the audio file from `tracks.file_path` on disk
  3. Upload to `track-audio/{uid}/{track_hash}/audio.{ext}`
  4. Update local `tracks.storage_path` with the cloud path
- **File:** `src-tauri/src/database/local/tracks.rs` — add `set_storage_path(pool, track_id, path)` function
- **Optimization:** Skip upload if `storage_path` is already set (idempotent)

### Task 1.4: Upload stems on sync
- **File:** `src-tauri/src/services/cloud_sync.rs` — add to `sync_track_stem()` or `sync_track_with_children()`
- **Flow:**
  1. After upserting stem metadata, check if `track_stems.storage_path` is NULL
  2. Read stem file from `track_stems.file_path` on disk
  3. Upload to `track-stems/{uid}/{track_hash}/stems/{stem_name}.{ext}`
  4. Update local `track_stems.storage_path` with cloud path
- **File:** `src-tauri/src/database/local/tracks.rs` — add `set_stem_storage_path(pool, track_id, stem_name, path)` function
- **Optimization:** Upload all 4 stems in parallel (tokio::join!)

### Task 1.5: Download track audio from cloud
- **File:** New function in `src-tauri/src/services/cloud_sync.rs` or a new `src-tauri/src/services/cloud_pull.rs`
- **Function:** `download_track_audio(client, pool, app_handle, track_remote_id, access_token) -> Result<(), Error>`
- **Flow:**
  1. Fetch track metadata from Supabase (need `storage_path`, `track_hash`)
  2. Download audio bytes from `track-audio/{storage_path}`
  3. Write to local filesystem at standard location: `tracks/{uuid}.{ext}`
  4. Update local `tracks.file_path` with the local path
- **Dependency:** Task 1.2 (storage client)

### Task 1.6: Download stems from cloud
- **File:** Same as 1.5
- **Function:** `download_track_stems(client, pool, app_handle, track_remote_id, access_token) -> Result<(), Error>`
- **Flow:**
  1. Fetch stem metadata for track from Supabase (need `storage_path`, `stem_name`)
  2. Download each stem file (parallel)
  3. Write to `tracks/stems/{track_hash}/{stem_name}.{ext}`
  4. Upsert `track_stems` rows locally with correct `file_path`
- **Dependency:** Task 1.2

---

## Epic 2: Share Codes (No Dependencies)

Venue owners generate a short, memorable code that DJs enter to join the venue.

### Task 2.1: Add `share_code` column to venues
- **File:** New migration `src-tauri/migrations/YYYYMMDDHHMMSS_add_venue_share_code.sql`
- **SQL:**
  ```sql
  ALTER TABLE venues ADD COLUMN share_code TEXT UNIQUE;
  ```
- **Note:** Nullable — only populated on demand when owner generates it.

### Task 2.2: Add `share_code` to Supabase venues table
- **Where:** Supabase dashboard / migration
- **SQL:** `ALTER TABLE venues ADD COLUMN share_code TEXT UNIQUE;`
- Must be synced from local → cloud when generated.

### Task 2.3: Share code generation function
- **File:** `src-tauri/src/services/venues.rs` (new file or add to existing)
- **Function:** `generate_share_code() -> String`
- **Spec:** 8 characters, base62 (a-z, A-Z, 0-9), no underscores, no hyphens. Example: `a3kX9mBv`
- **Implementation:** Use `rand` crate to pick 8 random chars from the base62 alphabet.
- **Collision handling:** Generate, attempt INSERT, retry on UNIQUE violation (astronomically unlikely with 62^8 = 218 trillion combinations).

### Task 2.4: Tauri command to generate/get share code
- **File:** `src-tauri/src/commands/venues.rs`
- **Command:** `get_or_create_share_code(venue_id: i64) -> Result<String, String>`
- **Flow:**
  1. Check if venue already has a `share_code` — if so, return it
  2. Verify caller owns the venue (`venues.uid == current_user_id`)
  3. Generate code via Task 2.3
  4. Save to local DB: `UPDATE venues SET share_code = ? WHERE id = ?`
  5. Sync to Supabase (update the venue's cloud record with the share_code)
  6. Return the code
- **File:** `src-tauri/src/database/local/venues.rs` — add `set_share_code(pool, venue_id, code)` and `get_share_code(pool, venue_id)`

### Task 2.5: Update venue sync to include share_code
- **File:** `src-tauri/src/database/remote/venues.rs`
- **Change:** Add `share_code: Option<&'a str>` to `VenuePayload` struct. Include in upsert.
- **File:** `src-tauri/src/models/venues.rs`
- **Change:** Add `pub share_code: Option<String>` to `Venue` struct.
- **File:** `src-tauri/src/database/local/venues.rs`
- **Change:** Update all SELECT queries to include `share_code` column.

### Task 2.6: Share venue UI
- **File:** New component `src/features/venues/components/share-venue-dialog.tsx`
- **UI:** Button in venue header area (next to close venue button). Opens a popover/dialog showing:
  - The share code in large monospace text
  - "Copy" button (copies to clipboard)
  - "Code expires never" or similar reassurance
- **Trigger:** Only visible when `currentVenue.uid === currentUserId` (owner only)
- **File:** `src/App.tsx` — add share button to venue header tabs area

---

## Epic 3: Join Venue (Depends on Epic 2)

DJs enter a share code to join a venue. This creates a membership record in Supabase and pulls the venue config into the DJ's local database.

### Task 3.1: Create `venue_members` table in Supabase
- **Where:** Supabase dashboard / migration
- **Schema:**
  ```sql
  CREATE TABLE venue_members (
    id BIGSERIAL PRIMARY KEY,
    venue_id BIGINT NOT NULL REFERENCES venues(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(venue_id, user_id)
  );
  ```
- **RLS policies:**
  - SELECT: user can see memberships for venues they own OR are a member of
  - INSERT: any authenticated user can insert a row for themselves (self-join)
  - DELETE: venue owner can remove members; member can remove self

### Task 3.2: Supabase RPC to join by share code
- **Where:** Supabase SQL function
- **Function:** `join_venue_by_code(code TEXT)`
- **Logic:**
  1. Look up venue by `share_code = code`
  2. If not found → error "Invalid code"
  3. If caller is already a member → return venue data (idempotent)
  4. Insert into `venue_members(venue_id, user_id)` with `auth.uid()`
  5. Return venue row (id, name, description, share_code, uid)
- **Why RPC:** Avoids exposing the venues table to arbitrary reads by share_code. The function controls what's returned.

### Task 3.3: Add `role` column to local venues table
- **File:** New migration `src-tauri/migrations/YYYYMMDDHHMMSS_add_venue_role.sql`
- **SQL:**
  ```sql
  ALTER TABLE venues ADD COLUMN role TEXT NOT NULL DEFAULT 'owner';
  ```
- **Values:** `'owner'` (created locally), `'member'` (joined via code)
- **File:** `src-tauri/src/models/venues.rs` — add `pub role: String` to `Venue` struct
- **File:** `src-tauri/src/database/local/venues.rs` — update all SELECT queries to include `role`

### Task 3.4: Tauri command to join venue by code
- **File:** `src-tauri/src/commands/venues.rs`
- **Command:** `join_venue(code: String) -> Result<Venue, String>`
- **Flow:**
  1. Call Supabase RPC `join_venue_by_code(code)` — returns venue metadata + remote venue ID
  2. Check if venue already exists locally (by `remote_id`) — if so, return it
  3. Insert venue into local DB with `role = 'member'`, `remote_id` set to the cloud ID
  4. Pull venue children (Tasks 3.5-3.6)
  5. Return the local Venue record
  6. Navigate to `/venue/{id}/universe`

### Task 3.5: Pull fixtures for a venue from Supabase
- **File:** New module `src-tauri/src/services/cloud_pull.rs` (or extend `cloud_sync.rs`)
- **Function:** `pull_venue_fixtures(client, pool, venue_remote_id, access_token) -> Result<(), Error>`
- **Flow:**
  1. `client.select("fixtures", "venue_id=eq.{venue_remote_id}&select=*", token)` — fetch all fixtures
  2. For each fixture: insert into local `fixtures` table with `venue_id` mapped to local venue ID, `remote_id` set
  3. Preserve UUIDs (`fixtures.id` is a UUID string, not autoincrement — use the same ID)
- **Deserialization:** Need a `RemoteFixture` struct matching Supabase column names (snake_case), then map to local `PatchedFixture`

### Task 3.6: Pull groups, group members, tags, tag assignments
- **File:** Same `cloud_pull.rs`
- **Functions:**
  - `pull_venue_groups(client, pool, venue_remote_id, local_venue_id, access_token)`
  - `pull_venue_tags(client, pool, venue_remote_id, local_venue_id, access_token)`
- **Flow for groups:**
  1. Fetch `fixture_groups` where `venue_id = remote_venue_id`
  2. Insert locally, map `remote_id` → `local_id` for groups
  3. Fetch `fixture_group_members` for those group IDs
  4. Insert locally with mapped fixture IDs and group IDs
- **Flow for tags:**
  1. Fetch `fixture_tags` where `venue_id = remote_venue_id`
  2. Insert locally
  3. Fetch `fixture_tag_assignments` for those tag IDs
  4. Insert locally with mapped fixture IDs

### Task 3.7: Join venue UI
- **File:** New component `src/features/venues/components/join-venue-dialog.tsx`
- **UI:** Dialog with:
  - Text input for the 8-character code (could use InputOTP component for nice UX)
  - "Join" button
  - Loading state during pull
  - Error state for invalid code
- **Trigger:** Button on WelcomeScreen next to "Create Venue" — labeled "Join Venue"
- **File:** `src/features/venues/components/venue-list.tsx` — add join button

### Task 3.8: Read-only universe designer for members
- **File:** `src/features/universe/components/universe-designer.tsx` (or wrapper)
- **Change:** Pass `readOnly` prop when `currentVenue.role === 'member'`
- **Behavior:** Hide patch/unpatch buttons, disable drag-to-move fixtures, disable address editing, hide "Add Fixture" button. Show fixtures and groups but don't allow modifications.
- **Visual indicator:** Banner or badge showing "Read Only — owned by {owner_name}" or similar

---

## Epic 4: DJ Syncs Scores to Joined Venue (Depends on Epic 3)

DJs annotate tracks for a venue they've joined and sync those scores to the cloud. The existing sync flow handles this — the main changes are Supabase RLS and ensuring the DJ can write to a venue they don't own.

### Task 4.1: Update Supabase RLS for scores
- **Where:** Supabase dashboard
- **Current:** Scores can only be written by the venue owner (uid matches)
- **New policy:** Allow INSERT/UPDATE on `scores` where `auth.uid()` is either:
  - The venue owner (`venues.uid = auth.uid()`), OR
  - A venue member (`EXISTS (SELECT 1 FROM venue_members WHERE venue_id = scores.venue_id AND user_id = auth.uid())`)
- **Same for:** `track_scores` (via score's venue_id join)

### Task 4.2: Update Supabase RLS for patterns and implementations
- **Where:** Supabase dashboard
- **Current:** Users can only write their own patterns
- **No change needed** — DJs create patterns under their own `uid`. The patterns are globally readable (for community features). The DJ's patterns just happen to be referenced by scores on a shared venue.

### Task 4.3: Update Supabase RLS for tracks and track children
- **Where:** Supabase dashboard
- **Current:** Users can only write their own tracks
- **No change needed** — DJs sync their own tracks. The owner will read them later via venue-scoped score queries.

### Task 4.4: Ensure DJ sync includes venue_id correctly
- **File:** Verify `src-tauri/src/services/cloud_sync.rs` — `sync_score()`
- **Check:** When a DJ creates a score for a joined venue, `score.venue_id` points to the local venue row (which has `remote_id` set from the join). The sync flow already resolves `venue_remote_id` from the local venue's `remote_id`. Should work unchanged.
- **Potential issue:** The DJ's local venue has `role = 'member'` and its `uid` is the owner's UID (from the cloud venue record). Make sure the sync doesn't filter by `uid = current_user` for venues — it should sync all venues regardless of role.

---

## Epic 5: Owner Pulls All Venue Data (Depends on Epic 1 + Epic 4)

This is the core of the feature. The venue owner pulls all scores from all DJs, along with the patterns, implementations, tracks, audio, stems, beats, and roots those scores depend on.

### Task 5.1: Supabase query — all scores for a venue
- **File:** New functions in `src-tauri/src/database/remote/` (new module `pull.rs` or extend `scores.rs`)
- **Function:** `fetch_scores_for_venue(client, venue_remote_id, access_token) -> Result<Vec<RemoteScore>, SyncError>`
- **Query:** `client.select("scores", "venue_id=eq.{venue_remote_id}&select=id,uid,track_id,venue_id,name,created_at,updated_at", token)`
- **Returns all scores** regardless of `uid` — this is the key difference from existing sync which is user-scoped.

### Task 5.2: Supabase query — track_scores for a score
- **Function:** `fetch_track_scores_for_score(client, score_remote_id, access_token) -> Result<Vec<RemoteTrackScore>, SyncError>`
- **Query:** `client.select("track_scores", "score_id=eq.{score_remote_id}&select=*", token)`

### Task 5.3: Supabase query — track metadata by remote ID
- **Function:** `fetch_track(client, track_remote_id, access_token) -> Result<RemoteTrack, SyncError>`
- **Query:** `client.select("tracks", "id=eq.{track_remote_id}&select=*", token)`
- **Purpose:** Get title, artist, album, duration, hash, storage_path for stub track creation

### Task 5.4: Supabase query — pattern + implementation by remote ID
- **Function:** Already partially exists: `fetch_implementation_by_pattern()` in `src-tauri/src/database/remote/implementations.rs`
- **Also need:** `fetch_pattern(client, pattern_remote_id, access_token)` for pattern metadata (name, category, is_published)
- **File:** Extend `src-tauri/src/database/remote/patterns.rs`

### Task 5.5: Supabase query — beats and roots for a track
- **Function:** `fetch_track_beats(client, track_remote_id, access_token)` and `fetch_track_roots(client, track_remote_id, access_token)`
- **File:** Extend `src-tauri/src/database/remote/track_beats.rs` and `track_roots.rs`
- **Data:** Beats are `{bpm, beats_json, downbeats_json, downbeat_offset}`. Roots are `{sections_json}`.

### Task 5.6: Pull orchestrator — pull all data for a venue
- **File:** New `src-tauri/src/services/cloud_pull.rs`
- **Function:** `pull_venue_scores(client, pool, app_handle, venue_local_id, access_token) -> Result<PullStats, Error>`
- **Flow:**
  1. Get venue's `remote_id` from local DB
  2. Fetch all scores for the venue (Task 5.1)
  3. For each score:
     a. **Track:** Check if track exists locally by `remote_id`. If not:
        - Fetch track metadata (Task 5.3)
        - Create stub track row locally (title, artist, album, duration, hash, remote_id, uid). Set `file_path` to empty or placeholder.
        - Download audio file (Task 1.5) → update `file_path`
        - Download stems (Task 1.6) → create `track_stems` rows with `file_path`
        - Fetch + insert beats (Task 5.5)
        - Fetch + insert roots (Task 5.5)
     b. **Score:** Check if score exists locally by `remote_id`. If not:
        - Insert score row locally (track_id mapped to local, venue_id = local venue ID, remote_id set)
     c. **Track Scores:** Fetch track_scores for this score (Task 5.2). For each:
        - **Pattern:** Check if pattern exists locally by `remote_id`. If not:
          - Fetch pattern metadata (Task 5.4)
          - Insert pattern row locally (remote_id set, uid = DJ's uid)
        - **Implementation:** Check if implementation exists locally for this pattern. If not:
          - Fetch implementation (Task 5.4, already exists)
          - Insert implementation row locally (graph_json, remote_id set)
        - Insert track_score row locally (score_id, pattern_id mapped to local IDs, remote_id set)
  4. Return stats (scores pulled, tracks created, patterns fetched, etc.)

### Task 5.7: Handle updates on re-pull
- **In the pull orchestrator (Task 5.6):**
- For records that already exist locally (matched by `remote_id`):
  - **Scores:** Update `name`, `updated_at` if cloud version is newer
  - **Track scores:** Delete all local track_scores for the score, re-insert from cloud (same strategy as push sync uses)
  - **Patterns:** Update `name` if changed
  - **Implementations:** Update `graph_json` if cloud `updated_at` is newer (this handles DJ updating their pattern)
  - **Tracks:** Update metadata (title, artist) if changed
  - **Beats/roots:** Overwrite if cloud version newer
- **Key:** Compare `updated_at` timestamps to avoid overwriting newer local data

### Task 5.8: Tauri command to pull venue data
- **File:** `src-tauri/src/commands/cloud_sync.rs`
- **Command:** `pull_venue_data(venue_id: i64) -> Result<PullStats, String>`
- **Flow:**
  1. `require_auth()`
  2. Create SupabaseClient
  3. Call `cloud_pull::pull_venue_fixtures()` (refresh fixtures/groups from owner)
  4. Call `cloud_pull::pull_venue_scores()` (all scores + deps)
  5. Return stats

### Task 5.9: Pull on app launch
- **File:** `src/App.tsx` — in the init block where `sync_all()` is called
- **Change:** After `sync_all()`, for each local venue where `role = 'member'`, call `pull_venue_data(venue_id)`. For venues where `role = 'owner'`, also pull (to get scores from member DJs).
- **Alternative:** Only pull for the `currentVenue` if one is set, to avoid pulling all venues on every launch.

### Task 5.10: Manual refresh button
- **File:** Add to venue header in `src/App.tsx` or a venue-level component
- **UI:** Refresh icon button (next to share/close buttons). Shows spinner while pulling.
- **Action:** Calls `invoke("pull_venue_data", { venueId: currentVenue.id })`
- **Visible to:** Both owner and members

---

## Epic 6: UI Polish & Edge Cases (Depends on Epic 5)

### Task 6.1: Pattern browser — hide foreign patterns
- **File:** `src/features/patterns/stores/use-patterns-store.ts`
- **Change:** The existing `filteredPatterns()` with `filter: "mine" | "community"` already handles this. Patterns pulled from DJs have `uid != current_user` and `is_published = false`, so they won't appear in either "mine" or "community" lists.
- **Verify:** Confirm that unpublished patterns with foreign `uid` are excluded. If not, add explicit filter: `patterns.filter(p => p.uid === currentUserId || p.isPublished)`.

### Task 6.2: Track browser — show all tracks, indicate remote
- **File:** `src/features/tracks/components/track-browser.tsx`
- **Change:** Stub tracks (from DJ imports) appear in the track list. Add a visual indicator for tracks where the audio was downloaded from cloud (e.g., a small cloud icon, or a different status dot color).
- **Optional:** Show tracks missing audio with a download button (for future track-by-track download). For now, the pull command downloads everything.

### Task 6.3: Perform mode — random score selection for duplicate tracks
- **File:** `src-tauri/src/commands/perform.rs` — `perform_match_track()`
- **Current:** `get_tracks_by_source_filename()` returns multiple tracks, picks `first()`.
- **Change:** When multiple tracks match and have scores for the current venue, pick one randomly (`rand::thread_rng().choose()`). Log which one was selected.
- **Future enhancement:** Track which scores have been played and prefer unplayed ones (noted but not implemented now).

### Task 6.4: Venue list — distinguish owned vs joined
- **File:** `src/features/venues/components/venue-list.tsx`
- **Change:** Venue cards for `role = 'member'` show a visual distinction:
  - Badge or icon indicating "Joined" vs "My Venue"
  - Maybe a subtle border color difference
  - Show owner name if available

### Task 6.5: Venue header — show role context
- **File:** `src/App.tsx` venue header tabs area
- **Change:** When viewing a joined venue, show "Member" badge. Share button only visible for owners. Universe tab shows "(Read Only)" for members.

### Task 6.6: Leave venue
- **File:** `src-tauri/src/commands/venues.rs`
- **Command:** `leave_venue(venue_id: i64) -> Result<(), String>`
- **Flow:**
  1. Delete from Supabase `venue_members` where user_id = current user
  2. Delete local venue + cascade (fixtures, groups, scores all cleaned up by FK cascade)
- **UI:** Context menu or button on joined venue card

---

## Supabase Schema Summary

New tables/columns needed on Supabase:

```sql
-- On existing venues table
ALTER TABLE venues ADD COLUMN share_code TEXT UNIQUE;

-- New table
CREATE TABLE venue_members (
  id BIGSERIAL PRIMARY KEY,
  venue_id BIGINT NOT NULL REFERENCES venues(id) ON DELETE CASCADE,
  user_id UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
  joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(venue_id, user_id)
);

-- RPC function
CREATE OR REPLACE FUNCTION join_venue_by_code(code TEXT)
RETURNS venues AS $$
DECLARE
  v venues%ROWTYPE;
BEGIN
  SELECT * INTO v FROM venues WHERE share_code = code;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Invalid venue code';
  END IF;
  INSERT INTO venue_members (venue_id, user_id)
  VALUES (v.id, auth.uid())
  ON CONFLICT (venue_id, user_id) DO NOTHING;
  RETURN v;
END;
$$ LANGUAGE plpgsql SECURITY DEFINER;

-- Storage buckets
-- track-audio (public read for authenticated, write for owner)
-- track-stems (public read for authenticated, write for owner)

-- RLS updates:
-- scores: allow INSERT/UPDATE where user is venue member
-- track_scores: allow INSERT/UPDATE where user is venue member (via score join)
-- venue_members: allow self-insert, owner-delete, self-delete
```

---

## Local Schema Migration Summary

```sql
-- Migration: add_venue_share_code
ALTER TABLE venues ADD COLUMN share_code TEXT UNIQUE;

-- Migration: add_venue_role
ALTER TABLE venues ADD COLUMN role TEXT NOT NULL DEFAULT 'owner';
```

---

## Dependency Graph

```
Epic 0 (stems FLAC)          ─── independent, do anytime ───

Epic 1 (storage)             Epic 2 (share codes)
  1.1 buckets                  2.1 local migration
  1.2 storage client           2.2 supabase migration
  1.3 upload audio             2.3 code generation
  1.4 upload stems             2.4 tauri command
  1.5 download audio           2.5 sync share_code
  1.6 download stems           2.6 share UI
       │                            │
       │                            ▼
       │                       Epic 3 (join venue)
       │                         3.1 venue_members table
       │                         3.2 join RPC
       │                         3.3 role column
       │                         3.4 join command
       │                         3.5 pull fixtures
       │                         3.6 pull groups/tags
       │                         3.7 join UI
       │                         3.8 read-only universe
       │                            │
       │                            ▼
       │                       Epic 4 (DJ sync scores)
       │                         4.1 RLS scores
       │                         4.2 RLS patterns
       │                         4.3 RLS tracks
       │                         4.4 verify sync flow
       │                            │
       ├────────────────────────────┤
       │                            │
       ▼                            ▼
            Epic 5 (owner pulls all)
              5.1-5.5 fetch queries
              5.6 pull orchestrator
              5.7 update handling
              5.8 tauri command
              5.9 pull on launch
              5.10 refresh button
                      │
                      ▼
              Epic 6 (UI polish)
                6.1 pattern filter
                6.2 track indicators
                6.3 random score pick
                6.4 venue list badges
                6.5 venue header role
                6.6 leave venue
```

## Task Count Summary

| Epic | Tasks | Effort |
|------|-------|--------|
| 0: Stem compression | 4 | Small (optional) |
| 1: Cloud storage | 6 | Medium |
| 2: Share codes | 6 | Small |
| 3: Join venue | 8 | Large |
| 4: DJ sync | 4 | Small (mostly RLS config) |
| 5: Owner pull | 10 | Large (critical path) |
| 6: UI polish | 6 | Medium |
| **Total** | **44** | |

## Key Files Modified

### Rust Backend
- `src-tauri/src/database/remote/common.rs` — storage client methods
- `src-tauri/src/database/remote/venues.rs` — share_code in payload
- `src-tauri/src/database/remote/scores.rs` — venue-scoped fetch
- `src-tauri/src/database/remote/patterns.rs` — fetch by remote_id
- `src-tauri/src/database/remote/implementations.rs` — already has fetch
- `src-tauri/src/database/remote/track_beats.rs` — fetch by track
- `src-tauri/src/database/remote/track_roots.rs` — fetch by track
- `src-tauri/src/database/local/venues.rs` — share_code, role, CRUD updates
- `src-tauri/src/database/local/tracks.rs` — storage_path setters
- `src-tauri/src/models/venues.rs` — share_code, role fields
- `src-tauri/src/services/cloud_sync.rs` — upload audio/stems
- `src-tauri/src/services/cloud_pull.rs` — **new file**, pull orchestrator
- `src-tauri/src/commands/venues.rs` — share_code, join, leave commands
- `src-tauri/src/commands/cloud_sync.rs` — pull_venue_data command
- `src-tauri/src/commands/perform.rs` — random score selection
- `src-tauri/src/lib.rs` — register new commands
- `src-tauri/python/audio_preprocessor.py` — FLAC output (Epic 0)
- `src-tauri/src/stem_worker.rs` — FLAC extensions (Epic 0)

### Frontend
- `src/App.tsx` — share button, refresh button, pull on launch, venue header role
- `src/features/venues/components/venue-list.tsx` — join button, role badges
- `src/features/venues/components/join-venue-dialog.tsx` — **new file**
- `src/features/venues/components/share-venue-dialog.tsx` — **new file**
- `src/features/universe/components/universe-designer.tsx` — read-only mode
- `src/features/tracks/components/track-browser.tsx` — remote track indicator
- `src/features/patterns/stores/use-patterns-store.ts` — verify foreign filter

### Migrations
- `src-tauri/migrations/YYYYMMDDHHMMSS_add_venue_share_code.sql` — **new**
- `src-tauri/migrations/YYYYMMDDHHMMSS_add_venue_role.sql` — **new**

### Supabase (Dashboard / SQL)
- `venue_members` table + RLS
- `join_venue_by_code` RPC function
- `share_code` column on venues
- Storage buckets: `track-audio`, `track-stems`
- RLS updates on `scores`, `track_scores`
