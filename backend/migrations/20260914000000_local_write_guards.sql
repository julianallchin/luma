-- The local write guards on synced tables come off.
--
-- They were the old engine's row-level security, enforced twice: once in the
-- app and once again in a BEFORE trigger reading `auth_write_admission`. With
-- PowerSync the second copy is not a safety net, it is a wall. A download runs
-- on the SDK's own connection and writes the app tables directly, in whatever
-- order the checkpoint delivers them — a membership before the venue it admits
-- to, a clip before its score, a message before its parent. Every one of these
-- triggers would abort that write, and the row would simply never arrive.
--
-- What replaces them:
--
--   * Postgres row-level security decides who may read and write a row. That
--     is the authority, and it is the one a second device cannot bypass.
--   * The app validates before it writes: `VenueAccess` still takes the lease
--     and still refuses a venue this principal may not touch, and
--     `AppServices::require_session` refuses a synced-table write with nobody
--     signed in.
--
-- What stays: single-row integrity that does not depend on another row or on
-- who is signed in — a fixture's address fitting its universe, an identity
-- column that no device may rewrite. Those are true of a downloaded row as
-- much as a local one.

DROP TRIGGER auth_venue_insert;
DROP TRIGGER auth_venue_update;
DROP TRIGGER auth_venue_delete;

DROP TRIGGER auth_venue_membership_insert;
DROP TRIGGER auth_venue_membership_update;
DROP TRIGGER auth_venue_membership_delete;

DROP TRIGGER auth_venue_fixture_insert;
DROP TRIGGER auth_venue_fixture_update;
DROP TRIGGER auth_venue_fixture_delete;

DROP TRIGGER auth_venue_group_insert;
DROP TRIGGER auth_venue_group_update;
DROP TRIGGER auth_venue_group_delete;

DROP TRIGGER auth_venue_group_member_insert;
DROP TRIGGER auth_venue_group_member_update;
DROP TRIGGER auth_venue_group_member_delete;

DROP TRIGGER auth_venue_node_insert;
DROP TRIGGER auth_venue_node_update;
DROP TRIGGER auth_venue_node_delete;

DROP TRIGGER auth_venue_edge_insert;
DROP TRIGGER auth_venue_edge_update;
DROP TRIGGER auth_venue_edge_delete;

DROP TRIGGER auth_venue_param_insert;
DROP TRIGGER auth_venue_param_update;
DROP TRIGGER auth_venue_param_delete;

DROP TRIGGER auth_venue_constraint_insert;
DROP TRIGGER auth_venue_constraint_update;
DROP TRIGGER auth_venue_constraint_delete;

DROP TRIGGER auth_venue_cue_insert;
DROP TRIGGER auth_venue_cue_update;
DROP TRIGGER auth_venue_cue_delete;

DROP TRIGGER auth_venue_midi_modifier_insert;
DROP TRIGGER auth_venue_midi_modifier_update;
DROP TRIGGER auth_venue_midi_modifier_delete;

DROP TRIGGER auth_venue_midi_binding_insert;
DROP TRIGGER auth_venue_midi_binding_update;
DROP TRIGGER auth_venue_midi_binding_delete;

DROP TRIGGER auth_venue_score_insert;
DROP TRIGGER auth_venue_score_update;
DROP TRIGGER auth_venue_score_delete;

DROP TRIGGER auth_admit_tracks_insert;
DROP TRIGGER auth_admit_tracks_update;
DROP TRIGGER auth_admit_tracks_delete;

DROP TRIGGER auth_admit_track_beats_insert;
DROP TRIGGER auth_admit_track_beats_update;
DROP TRIGGER auth_admit_track_beats_delete;

DROP TRIGGER auth_admit_track_roots_insert;
DROP TRIGGER auth_admit_track_roots_update;
DROP TRIGGER auth_admit_track_roots_delete;

DROP TRIGGER auth_admit_track_stems_insert;
DROP TRIGGER auth_admit_track_stems_update;
DROP TRIGGER auth_admit_track_stems_delete;

DROP TRIGGER auth_admit_track_drum_onsets_insert;
DROP TRIGGER auth_admit_track_drum_onsets_update;
DROP TRIGGER auth_admit_track_drum_onsets_delete;

DROP TRIGGER auth_admit_track_bar_classifications_insert;
DROP TRIGGER auth_admit_track_bar_classifications_update;
DROP TRIGGER auth_admit_track_bar_classifications_delete;

DROP TRIGGER auth_admit_patterns_insert;
DROP TRIGGER auth_admit_patterns_update;
DROP TRIGGER auth_admit_patterns_delete;

DROP TRIGGER auth_admit_implementations_insert;
DROP TRIGGER auth_admit_implementations_update;
DROP TRIGGER auth_admit_implementations_delete;

-- Conversations: the same gate, spelled per table.
DROP TRIGGER agent_thread_insert_requires_active_admission;
DROP TRIGGER agent_thread_update_requires_active_admission;
DROP TRIGGER agent_thread_delete_requires_active_admission;
DROP TRIGGER agent_thread_message_insert_requires_owner_admission;
DROP TRIGGER agent_thread_message_delete_requires_owner_admission;
DROP TRIGGER agent_thread_head_insert_requires_owner_admission;
DROP TRIGGER agent_thread_head_update_requires_owner_admission;
DROP TRIGGER agent_thread_head_delete_requires_owner_admission;
DROP TRIGGER agent_thread_transcript_head_matches_owner_insert;
DROP TRIGGER agent_thread_transcript_head_matches_owner_update;

-- Ordering hazards: each of these requires a *different* row to be present
-- already, which a download cannot promise.
DROP TRIGGER agent_thread_message_requires_valid_parent;
DROP TRIGGER agent_thread_parent_shares_its_owner;
DROP TRIGGER pattern_score_owner_insert;
DROP TRIGGER pattern_score_owner_update;

-- A downloaded thread may arrive after its transcript head, so seeding the
-- empty head becomes a no-op rather than a conflict.
DROP TRIGGER agent_thread_create_empty_transcript;
CREATE TRIGGER agent_thread_create_empty_transcript
AFTER INSERT ON agent_threads FOR EACH ROW
BEGIN
    INSERT OR IGNORE INTO agent_thread_transcript_heads (thread_id, uid, created_at, updated_at)
    VALUES (NEW.id, NEW.uid, strftime('%Y-%m-%dT%H:%M:%fZ','now'),
            strftime('%Y-%m-%dT%H:%M:%fZ','now'));
END;
