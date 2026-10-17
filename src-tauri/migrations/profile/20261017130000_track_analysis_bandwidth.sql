-- Where a file's high frequencies stop (`waveflow_core::analysis::bandwidth`),
-- so a lossless file made from a lossy one can be told. `cutoff_hz` is NULL
-- for a spectrum with no wall; `bandwidth_version` is NULL until the sweep has
-- measured the row, which it does for lossless files alone.
ALTER TABLE track_analysis ADD COLUMN cutoff_hz REAL;
ALTER TABLE track_analysis ADD COLUMN bandwidth_version INTEGER;
