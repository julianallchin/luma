-- Store the actual decoded audio duration with the waveform data.
-- This ensures the frontend time-to-bucket mapping matches the true audio length
-- rather than relying on metadata duration (which can differ due to encoder padding).
ALTER TABLE track_waveforms ADD COLUMN decoded_duration REAL;
