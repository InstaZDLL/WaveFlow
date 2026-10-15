-- Analysis version 2 (`waveflow_core::analysis::rhythm`): a confidence for
-- the tempo, and an energy measure that loudness normalisation does not
-- erase. Both NULL until the sweep re-measures the row, which it does for
-- every row below `ANALYSIS_VERSION` 2.
ALTER TABLE track_analysis ADD COLUMN bpm_confidence REAL;
ALTER TABLE track_analysis ADD COLUMN energy REAL;
