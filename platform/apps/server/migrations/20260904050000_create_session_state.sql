-- Private harness data survives follow-up runs, but is never copied by history forks.
create table session_state (
    project_id uuid not null,
    session_id uuid not null,
    namespace text collate "C" not null check (namespace ~ '^[a-z][a-z0-9_.-]{0,127}$'),
    key text collate "C" not null check (octet_length(key) between 1 and 256 and key ~ '^[!-~]+$'),
    version bigint not null check (version > 0),
    value jsonb check (value is null or jsonb_typeof(value) = 'object'),
    saved_by_run_id uuid not null,
    saved_by_lease_epoch bigint not null check (saved_by_lease_epoch > 0),
    created_at timestamptz not null default clock_timestamp(),
    updated_at timestamptz not null default clock_timestamp(),
    primary key (session_id, namespace, key),
    foreign key (project_id, session_id) references sessions(project_id, id) on delete restrict,
    foreign key (project_id, session_id, saved_by_run_id) references runs(project_id, session_id, id) on delete restrict
);
-- Covers both foreign keys; the primary key supports namespace/key pagination.
create index session_state_writer_idx on session_state(project_id, session_id, saved_by_run_id);

create function runtime_guard_session_state() returns trigger language plpgsql as $$
declare r runs%rowtype;
begin
    -- Match the runtime's session -> run -> private state lock order.
    perform 1 from sessions where id = new.session_id for update;
    select * into r from runs where id = new.saved_by_run_id for update;
    if not found then raise exception 'run not found' using errcode = '23503'; end if;
    if r.session_id <> new.session_id or r.project_id <> new.project_id
        or r.status <> 'running' or r.lease_expires_at is null
        or r.lease_expires_at <= clock_timestamp() or r.lease_epoch <> new.saved_by_lease_epoch
        or not exists(select 1 from workers w where w.id=r.worker_id and w.status<>'offline') then
        raise exception 'session state requires current execution ownership in the same session' using errcode = '23514';
    end if;
    if (tg_op = 'INSERT' and new.version <> 1) or
        (tg_op = 'UPDATE' and (
            new.project_id <> old.project_id or new.session_id <> old.session_id
            or new.namespace <> old.namespace or new.key <> old.key
            or new.created_at <> old.created_at or new.version <> old.version + 1)) then
        raise exception 'session state identity is immutable and writes require the next version' using errcode = '23514';
    end if;
    new.updated_at := clock_timestamp();
    return new;
end;
$$;
create trigger session_state_guard before insert or update on session_state
    for each row execute function runtime_guard_session_state();
-- Keep tombstones so delete/recreate never resets the optimistic version (ABA).
create trigger session_state_no_delete before delete on session_state
    for each row execute function runtime_reject_change();
comment on table session_state is 'Latest namespaced, harness-owned session state. NULL value is a versioned tombstone. Not transcript or forked history.';
