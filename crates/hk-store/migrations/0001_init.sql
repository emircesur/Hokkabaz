-- Key/value app settings. Values are JSON text; never store secrets here.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- Index of secret names held by the active SecretStore backend. The secret
-- value itself never touches SQLite; this only makes names listable.
CREATE TABLE secret_refs (
    name       TEXT PRIMARY KEY NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;
