-- Nothing reads the per-bucket RGB colors any more; the waveform is drawn
-- from the 3-band envelopes. track_waveforms is local-only, so there is no
-- matching Postgres migration.
ALTER TABLE track_waveforms DROP COLUMN colors_blob;
ALTER TABLE track_waveforms DROP COLUMN preview_colors_blob;
