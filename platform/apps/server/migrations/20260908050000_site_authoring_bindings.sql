-- One site per session; many sessions may edit the same site.
CREATE TABLE site_authoring_bindings (
 session_id uuid PRIMARY KEY REFERENCES sessions(id),
 site_id uuid NOT NULL REFERENCES project_sites(id),
 project_id uuid NOT NULL REFERENCES projects(project_id),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 FOREIGN KEY(project_id,session_id) REFERENCES sessions(project_id,id),
 FOREIGN KEY(project_id,site_id) REFERENCES project_sites(project_id,id)
);
CREATE TABLE site_authoring_calls (
 run_id uuid NOT NULL REFERENCES runs(id), operation_key text NOT NULL,
 site_id uuid NOT NULL REFERENCES project_sites(id), method text NOT NULL,
 request jsonb NOT NULL, operation_id uuid NOT NULL UNIQUE,
 PRIMARY KEY(run_id,operation_key)
);
