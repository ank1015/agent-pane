-- Declarations are optional. Existing sessions retain the empty legacy contract.
alter table harnesses add column harness_contract jsonb not null default '{}'
    check (jsonb_typeof(harness_contract) = 'object');
alter table sessions add column harness_contract jsonb not null default '{}'
    check (jsonb_typeof(harness_contract) = 'object');

create function runtime_freeze_harness_contract() returns trigger language plpgsql as $$
begin
    if tg_op = 'INSERT' then
        select harness_contract into new.harness_contract from harnesses
            where id = new.harness_id for share;
    elsif new.harness_contract is distinct from old.harness_contract then
        raise exception 'session harness contract is immutable' using errcode = '23514';
    end if;
    return new;
end;
$$;
create trigger sessions_contract_snapshot before insert or update of harness_contract on sessions
    for each row execute function runtime_freeze_harness_contract();

create table run_outputs (
    project_id uuid not null,
    session_id uuid not null,
    run_id uuid not null,
    sequence bigint not null check (sequence between 1 and 64),
    name text collate "C" not null check (name ~ '^[a-zA-Z][a-zA-Z0-9_.-]{0,127}$'),
    output jsonb not null check (jsonb_typeof(output) = 'object'),
    saved_by_lease_epoch bigint not null check (saved_by_lease_epoch > 0),
    created_at timestamptz not null default clock_timestamp(),
    primary key (run_id, name),
    unique (run_id, sequence),
    foreign key (project_id, session_id, run_id) references runs(project_id, session_id, id) on delete restrict
);
create index run_outputs_project_idx on run_outputs(project_id, session_id, run_id);
create trigger run_outputs_immutable before update or delete on run_outputs
    for each row execute function runtime_reject_change();

create function runtime_guard_run_output() returns trigger language plpgsql as $$
declare r runs%rowtype;
begin
    perform 1 from sessions where id = new.session_id for update;
    select * into r from runs where id = new.run_id for update;
    if not found then raise exception 'run not found' using errcode = '23503'; end if;
    if r.project_id <> new.project_id or r.session_id <> new.session_id
        or r.status <> 'running' or r.lease_epoch <> new.saved_by_lease_epoch
        or r.lease_expires_at is null or r.lease_expires_at <= clock_timestamp()
        or not exists(select 1 from workers where id=r.worker_id and status<>'offline') then
        raise exception 'output publication requires current execution ownership' using errcode = '23514';
    end if;
    return new;
end;
$$;
create trigger run_outputs_guard before insert on run_outputs
    for each row execute function runtime_guard_run_output();
comment on table run_outputs is 'Immutable harness-published outputs. No provisioning, resource lifetime guarantee, or execution grant is implied.';
