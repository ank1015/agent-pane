create table project_environments (
    id uuid primary key,
    project_id uuid not null references projects(project_id) on delete cascade,
    name text not null check (name = btrim(name) and char_length(name) between 1 and 128),
    type text not null check (type in ('machine', 'sandbox')),
    machine_id uuid,
    snapshot_id uuid,
    workspace_root text not null check (workspace_root = btrim(workspace_root) and char_length(workspace_root) between 1 and 128),
    path text not null check (char_length(path) between 1 and 4096),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint project_environments_target_valid check (
        (type = 'machine' and machine_id is not null and snapshot_id is null) or
        (type = 'sandbox' and snapshot_id is not null and machine_id is null)
    )
);

create index project_environments_project_list_idx
    on project_environments (project_id, lower(name), id);

-- Enforce immutable identity/type even for future writers outside the HTTP API.
create function update_project_environment_timestamp() returns trigger language plpgsql as $$
begin
    if new.id <> old.id or new.project_id <> old.project_id or new.type <> old.type then
        raise exception 'environment identity and type are immutable' using errcode = '23514';
    end if;
    new.updated_at = greatest(clock_timestamp(), old.updated_at + interval '1 microsecond');
    new.created_at = old.created_at;
    return new;
end;
$$;

create trigger project_environments_updated before update on project_environments
    for each row execute function update_project_environment_timestamp();
