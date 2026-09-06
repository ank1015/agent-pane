ALTER TABLE sites ADD COLUMN active_release_id TEXT;
ALTER TABLE sites ADD COLUMN release_generation INTEGER NOT NULL DEFAULT 0 CHECK (release_generation >= 0);

CREATE TABLE bundles (
    site_id TEXT NOT NULL REFERENCES sites(id),
    kind TEXT NOT NULL CHECK (kind IN ('revision', 'release')),
    id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    descriptor TEXT NOT NULL CHECK (json_valid(descriptor)),
    status TEXT NOT NULL CHECK (status IN ('staging', 'ready', 'failed')),
    error_code TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (site_id, kind, id)
);

CREATE TABLE activation_receipts (
    site_id TEXT NOT NULL REFERENCES sites(id),
    operation_key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    response TEXT NOT NULL CHECK (json_valid(response)),
    PRIMARY KEY (site_id, operation_key)
);
