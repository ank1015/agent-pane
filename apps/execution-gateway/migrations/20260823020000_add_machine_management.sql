alter table machines
    add column deleted_at timestamptz;

create index machines_active_created_idx
    on machines(created_at, machine_id)
    where deleted_at is null;
