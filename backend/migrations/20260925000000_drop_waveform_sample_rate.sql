-- Every waveform is computed from the one 48 kHz decode (audio::SAMPLE_RATE),
-- so the column only ever held that constant. track_waveforms is local-only,
-- so there is no matching Postgres migration.
ALTER TABLE track_waveforms DROP COLUMN sample_rate;
