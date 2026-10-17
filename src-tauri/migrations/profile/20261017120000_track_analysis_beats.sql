-- Analysis version 4 (`waveflow_core::analysis::beats`): where the beats
-- fall, encoded by `beats::encode` (a format byte, the first beat in ms,
-- then each gap in ms). NULL until the sweep re-measures the row, and
-- for a track with no tempo.
ALTER TABLE track_analysis ADD COLUMN beats BLOB;
