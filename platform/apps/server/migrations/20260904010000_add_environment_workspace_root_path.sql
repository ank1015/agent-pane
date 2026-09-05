-- Preserve the execution root ID and store its last validated native path separately.
-- Existing records and snapshots without a host descriptor remain unknown until resolved.
alter table project_environments
    add column workspace_root_path text
        check (workspace_root_path is null or octet_length(workspace_root_path) between 1 and 4096);

comment on column project_environments.workspace_root is 'Execution gateway root ID used to address operations';
comment on column project_environments.workspace_root_path is 'Last validated native root path from the gateway; null when unknown';
