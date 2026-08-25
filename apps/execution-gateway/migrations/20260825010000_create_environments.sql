create table environments (
    environment_id text primary key,
    machine_id text not null references machines(machine_id) on delete restrict,
    workspace_root_id text not null,
    path text not null,
    created_at timestamptz not null default now(),
    deleted_at timestamptz,

    constraint environments_path_not_empty check (path <> '')
);

create unique index environments_active_location_unique
    on environments(machine_id, workspace_root_id, path)
    where deleted_at is null;

create index environments_machine_id_idx
    on environments(machine_id);

create index environments_active_machine_created_idx
    on environments(machine_id, created_at, environment_id)
    where deleted_at is null;

alter table machine_operations
    add column environment_id text references environments(environment_id) on delete restrict;

create index machine_operations_environment_created_idx
    on machine_operations(environment_id, created_at desc)
    where environment_id is not null;
