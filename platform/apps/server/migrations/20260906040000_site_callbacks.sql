-- Registration commits with run creation; every terminal transition creates an
-- outbox record in that same transaction, including worker/recovery failures.
create table site_callbacks (
    id uuid primary key,
    site_id uuid not null references project_sites(id),
    run_id uuid not null references runs(id),
    invocation_id uuid not null,
    release_id uuid not null,
    path text not null check (left(path,1)='/' and length(path) between 1 and 2048),
    payload jsonb not null,
    created_at timestamptz not null default now(),
    unique(site_id,run_id),
    foreign key(site_id,invocation_id) references site_invocations(site_id,id)
);
create index site_callbacks_run on site_callbacks(run_id);
create index site_callbacks_site on site_callbacks(site_id,id);
create table site_callback_deliveries (
    id uuid primary key default gen_random_uuid(),
    callback_id uuid not null unique references site_callbacks(id),
    event jsonb not null,
    status text not null default 'pending' check (status in ('pending','delivering','delivered','failed','cancelled')),
    attempts integer not null default 0 check (attempts>=0),
    version bigint not null default 0,
    next_attempt_at timestamptz not null default now(),
    lease_id uuid,
    lease_expires_at timestamptz,
    invocation_id uuid,
    last_invocation_id uuid,
    last_error text,
    finished_at timestamptz,
    created_at timestamptz not null default now(),
    check ((status='delivering')=(lease_id is not null and lease_expires_at is not null))
);
create index site_callback_delivery_due on site_callback_deliveries(next_attempt_at,id)
    where status in ('pending','delivering');

create function site_callback_scope() returns trigger language plpgsql as $$
begin
    perform 1 from runs r join project_sites s on s.project_id=r.project_id
        where r.id=new.run_id and s.id=new.site_id and r.status in ('ready','running','waiting') for key share of r,s;
    if not found then raise exception 'callback run is outside site project' using errcode='23503'; end if;
    return new;
end;
$$;
create trigger site_callback_scope before insert on site_callbacks
    for each row execute function site_callback_scope();

create function site_enqueue_callbacks() returns trigger language plpgsql as $$
begin
    if new.status in ('completed','failed','aborted') and old.status is distinct from new.status then
        insert into site_callback_deliveries(callback_id,event)
        select c.id,jsonb_build_object('type','run.'||new.status,'subscriptionId',c.id,
            'sessionId',new.session_id,'runId',new.id,'status',new.status,
            'finishedAt',new.finished_at,'payload',c.payload)
        from site_callbacks c where c.run_id=new.id
        on conflict(callback_id) do nothing;
    end if;
    return new;
end;
$$;
create trigger site_enqueue_callbacks after update of status on runs
    for each row execute function site_enqueue_callbacks();

-- Retained subscriptions pin release references; delivery scheduling must never
-- alter the identity or terminal snapshot replayed to application code.
create function site_guard_callback_identity() returns trigger language plpgsql as $$
begin
    if tg_table_name='site_callbacks' then
        raise exception 'callback subscriptions are immutable and retained' using errcode='23514';
    end if;
    if new.id is distinct from old.id or new.callback_id is distinct from old.callback_id
        or new.event is distinct from old.event or new.created_at is distinct from old.created_at then
        raise exception 'callback event identity is immutable' using errcode='23514';
    end if;
    return new;
end;
$$;
create trigger site_callbacks_immutable before update or delete on site_callbacks
    for each row execute function site_guard_callback_identity();
create trigger site_callback_event_immutable before update on site_callback_deliveries
    for each row execute function site_guard_callback_identity();
