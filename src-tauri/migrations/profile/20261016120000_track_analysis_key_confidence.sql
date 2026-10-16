-- Analysis version 3 (`waveflow_core::analysis::key`): the key read from
-- the audio goes in the existing `musical_key` column, and how far to
-- trust it here. NULL until the sweep re-measures the row.
ALTER TABLE track_analysis ADD COLUMN key_confidence REAL;
