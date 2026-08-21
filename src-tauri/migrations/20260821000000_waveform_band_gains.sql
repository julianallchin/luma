-- The units a track's band envelopes are in.
--
-- `bucketize_band_peaks` measures a raw peak per bucket per band; what is
-- stored in `bands_blob` / `preview_bands_blob` is that peak divided by the
-- band's 99th percentile over the whole track, then log-compressed and scaled.
-- Only the compressed result was ever kept, so the divisor — a whole-track
-- statistic no range contains — was unrecoverable, and
-- `get_track_waveform_window` could not answer in the same units the stored
-- envelope is drawn in. These three columns are that divisor.
--
-- Backfill is a recompute, not a patch: `get_track_waveform` treats a row with
-- no gains as a cache miss and republishes the whole payload through
-- `ensure_track_waveform`, so the envelopes and the units they are in always
-- come from one measurement of one decode. Old rows therefore stay readable and
-- heal the first time the track is opened; nothing here needs to guess a value
-- for them.
--
-- Not synced: `track_waveforms` is excluded from `sync::registry::TABLES`
-- (local/remote schema mismatch), and these columns are derived from the audio
-- like every other blob in the row.

ALTER TABLE track_waveforms ADD COLUMN band_gain_low REAL;
ALTER TABLE track_waveforms ADD COLUMN band_gain_mid REAL;
ALTER TABLE track_waveforms ADD COLUMN band_gain_high REAL;
