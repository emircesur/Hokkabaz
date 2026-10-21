-- n8n-shaped workflow persistence (P4.6). Additive: 0001 tables untouched.

-- `data` holds the n8n workflow JSON minus id/name/active (nodes, connections, settings,
-- staticData, pinData, unknown fields) so import/export stays lossless.
CREATE TABLE workflows (
    id         TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL,
    active     INTEGER NOT NULL DEFAULT 0,
    version_id TEXT NOT NULL,
    data       TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX workflows_updated ON workflows (updated_ms DESC);

-- Ids are n8n-style integers. `data` is the n8n runData envelope; the engine never puts
-- secrets in it (credential references only).
CREATE TABLE executions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    workflow_id   TEXT NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    mode          TEXT NOT NULL,
    status        TEXT NOT NULL,
    finished      INTEGER NOT NULL DEFAULT 0,
    started_ms    INTEGER NOT NULL,
    stopped_ms    INTEGER,
    workflow_data TEXT,
    data          TEXT
) STRICT;
CREATE INDEX executions_workflow ON executions (workflow_id, id DESC);
CREATE INDEX executions_status ON executions (status, id DESC);

-- Credential metadata only. The field values live in the SecretStore under
-- `credential.<id>` (one JSON blob per credential); they never touch SQLite.
CREATE TABLE credentials (
    id         TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL,
    type       TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
) STRICT;
