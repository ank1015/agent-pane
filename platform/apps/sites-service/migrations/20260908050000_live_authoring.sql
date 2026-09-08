CREATE TABLE authoring_operations (
 site_id TEXT NOT NULL REFERENCES sites(id), id TEXT NOT NULL,
 request TEXT NOT NULL, prepared TEXT NOT NULL, response TEXT,
 PRIMARY KEY(site_id,id)
);
CREATE TABLE site_snapshots (
 site_id TEXT NOT NULL REFERENCES sites(id), id TEXT NOT NULL,
 name TEXT NOT NULL, release_id TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
 PRIMARY KEY(site_id,id)
);
