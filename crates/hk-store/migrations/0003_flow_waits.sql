-- Durable Wait snapshots (hk-flow `WaitStore`). Additive: 0001/0002 untouched.
-- One row per parked execution; `record` is the serde JSON of hk-flow's `WaitRecord`.
-- Items and error messages only, never credential values (nodes hold references).
CREATE TABLE flow_waits (
    execution_id TEXT PRIMARY KEY NOT NULL,
    workflow_id  TEXT,
    -- unix ms: timer resume time, or the give-up time of a webhook wait; NULL = wait forever
    resume_at    INTEGER,
    record       TEXT NOT NULL,
    created_ms   INTEGER NOT NULL
) STRICT;
CREATE INDEX flow_waits_due ON flow_waits (resume_at) WHERE resume_at IS NOT NULL;
