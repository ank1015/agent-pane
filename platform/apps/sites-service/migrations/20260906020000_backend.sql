CREATE TABLE invocations (
    site_id TEXT NOT NULL REFERENCES sites(id),
    id TEXT NOT NULL,
    release_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','succeeded','failed','timed_out','interrupted')),
    response TEXT,
    error_code TEXT,
    logs TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    finished_at TEXT,
    PRIMARY KEY(site_id,id)
);
CREATE INDEX invocations_site_created ON invocations(site_id,created_at,id);
