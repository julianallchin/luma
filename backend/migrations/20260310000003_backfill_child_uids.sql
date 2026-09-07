-- Backfill uid on track child tables from parent track
UPDATE track_beats SET uid = (SELECT t.uid FROM tracks t WHERE t.id = track_beats.track_id)
WHERE uid IS NULL OR uid != (SELECT t.uid FROM tracks t WHERE t.id = track_beats.track_id);

UPDATE track_roots SET uid = (SELECT t.uid FROM tracks t WHERE t.id = track_roots.track_id)
WHERE uid IS NULL OR uid != (SELECT t.uid FROM tracks t WHERE t.id = track_roots.track_id);

UPDATE track_stems SET uid = (SELECT t.uid FROM tracks t WHERE t.id = track_stems.track_id)
WHERE uid IS NULL OR uid != (SELECT t.uid FROM tracks t WHERE t.id = track_stems.track_id);

UPDATE track_waveforms SET uid = (SELECT t.uid FROM tracks t WHERE t.id = track_waveforms.track_id)
WHERE uid IS NULL OR uid != (SELECT t.uid FROM tracks t WHERE t.id = track_waveforms.track_id);
