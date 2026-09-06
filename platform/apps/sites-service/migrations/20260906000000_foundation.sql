CREATE TABLE sites (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'provisioning'
        CHECK (status IN ('provisioning', 'ready', 'suspended', 'failed')),
    desired_status TEXT NOT NULL DEFAULT 'ready'
        CHECK (desired_status IN ('ready', 'suspended')),
    storage_initialized INTEGER NOT NULL DEFAULT 0
        CHECK (storage_initialized IN (0, 1)),
    error_code TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX sites_pending ON sites (storage_initialized, id);

-- Readiness exercises a committed write without touching site data.
CREATE TABLE storage_health (id INTEGER PRIMARY KEY CHECK (id = 1), value INTEGER NOT NULL);
INSERT INTO storage_health VALUES (1, 0);
