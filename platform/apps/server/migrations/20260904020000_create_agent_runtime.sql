-- Storage only: worker dispatch, authenticated lease ownership, and atomic SDK
-- operations are implemented separately. Project IDs repeated on association
-- rows allow scope to be enforced by composite foreign keys.

create table harnesses (
    id text primary key check (id ~ '^[a-z][a-z0-9_-]{0,127}$'),
    name text not null check (name = btrim(name) and char_length(name) between 1 and 128),
    description text,
    default_config jsonb not null default '{}' check (jsonb_typeof(default_config) = 'object'),
    config_schema jsonb check (jsonb_typeof(config_schema) = 'object'),
    enabled boolean not null default true,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now() check (updated_at >= created_at)
);

create table workers (
    id uuid primary key,
    build_id text not null check (build_id = btrim(build_id) and char_length(build_id) between 1 and 256),
    supported_harnesses text[] not null check (
        cardinality(supported_harnesses) > 0 and array_ndims(supported_harnesses) = 1
        and array_position(supported_harnesses, null) is null
        and array_position(supported_harnesses, '') is null
    ),
    status text not null default 'accepting' check (status in ('accepting', 'draining', 'offline')),
    capacity integer not null check (capacity > 0),
    started_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now() check (last_seen_at >= started_at)
);
create index workers_last_seen_idx on workers (last_seen_at) where status <> 'offline';

create table sessions (
    id uuid primary key,
    project_id uuid not null references projects(project_id) on delete restrict,
    harness_id text not null references harnesses(id) on delete restrict,
    title text check (title = btrim(title) and char_length(title) between 1 and 512),
    forked_from_session_id uuid,
    forked_at_revision bigint,
    current_revision bigint not null default 0 check (current_revision >= 0),
    created_at timestamptz not null default now(),
    last_activity_at timestamptz not null default now() check (last_activity_at >= created_at),
    archived_at timestamptz check (archived_at >= created_at),
    unique (project_id, id),
    foreign key (project_id, forked_from_session_id) references sessions(project_id, id) on delete restrict,
    constraint sessions_fork_valid check (
        (forked_from_session_id is null and forked_at_revision is null) or
        (forked_from_session_id is not null and forked_from_session_id <> id
            and forked_at_revision is not null and forked_at_revision >= 0)
    )
);
create index sessions_project_activity_idx on sessions (project_id, last_activity_at desc, id) where archived_at is null;
create index sessions_harness_idx on sessions (harness_id);
create index sessions_fork_idx on sessions (project_id, forked_from_session_id) where forked_from_session_id is not null;

create table runs (
    id uuid primary key,
    project_id uuid not null,
    session_id uuid not null,
    parent_run_id uuid,
    config jsonb not null default '{}' check (jsonb_typeof(config) = 'object'),
    status text not null default 'ready' check (status in ('ready', 'running', 'waiting', 'completed', 'failed', 'aborted')),
    version bigint not null default 1 check (version > 0),
    available_at timestamptz default now(),
    worker_id uuid references workers(id) on delete restrict,
    lease_epoch bigint not null default 0 check (lease_epoch >= 0),
    lease_expires_at timestamptz,
    abort_requested_at timestamptz,
    final_message_id uuid,
    error jsonb check (jsonb_typeof(error) = 'object'),
    last_event_sequence bigint not null default 0 check (last_event_sequence >= 0),
    created_at timestamptz not null default now(),
    started_at timestamptz check (started_at >= created_at),
    finished_at timestamptz check (finished_at >= coalesce(started_at, created_at)),
    unique (project_id, id),
    unique (project_id, session_id, id),
    foreign key (project_id, session_id) references sessions(project_id, id) on delete restrict,
    foreign key (project_id, parent_run_id) references runs(project_id, id) on delete restrict,
    check (parent_run_id is null or parent_run_id <> id),
    check (abort_requested_at is null or abort_requested_at >= created_at),
    constraint runs_schedule_valid check ((status = 'ready') = (available_at is not null)),
    constraint runs_lease_valid check (
        (status = 'running' and worker_id is not null and lease_expires_at is not null
            and lease_epoch > 0 and started_at is not null) or
        (status <> 'running' and worker_id is null and lease_expires_at is null)
    ),
    constraint runs_terminal_valid check ((status in ('completed', 'failed', 'aborted')) = (finished_at is not null)),
    constraint runs_final_message_valid check ((status = 'completed') = (final_message_id is not null)),
    constraint runs_failure_valid check ((status = 'failed') = (error is not null)),
    check (status <> 'aborted' or abort_requested_at is not null)
);
create unique index runs_one_live_per_session on runs (session_id) where status in ('ready', 'running', 'waiting');
create index runs_session_history_idx on runs (session_id, created_at, id);
create index runs_parent_idx on runs (project_id, parent_run_id) where parent_run_id is not null;
create index runs_ready_idx on runs (available_at, id) where status = 'ready';
create index runs_lease_expiry_idx on runs (lease_expires_at, id) where status = 'running';
create index runs_worker_idx on runs (worker_id) where worker_id is not null;

create table messages (
    id uuid primary key,
    project_id uuid not null references projects(project_id) on delete restrict,
    origin_run_id uuid,
    message jsonb not null,
    created_at timestamptz not null default now(),
    unique (project_id, id),
    foreign key (project_id, origin_run_id) references runs(project_id, id) on delete restrict,
    -- Full llm-contracts validation belongs to the write interface. Enforce the
    -- common envelope here, including object content for custom messages.
    constraint messages_envelope_valid check ((
        jsonb_typeof(message) = 'object'
        and message->>'role' in ('user', 'system', 'assistant', 'tool_result', 'custom')
        and jsonb_typeof(message->'content') = case when message->>'role' = 'custom' then 'object' else 'array' end
    ) is true)
);
create index messages_origin_run_idx on messages (project_id, origin_run_id) where origin_run_id is not null;

create table session_messages (
    project_id uuid not null,
    session_id uuid not null,
    revision bigint not null check (revision > 0),
    message_id uuid not null,
    run_id uuid,
    primary key (session_id, revision),
    unique (session_id, message_id),
    unique (project_id, session_id, run_id, message_id),
    foreign key (project_id, session_id) references sessions(project_id, id) on delete restrict,
    foreign key (project_id, message_id) references messages(project_id, id) on delete restrict,
    foreign key (project_id, session_id, run_id) references runs(project_id, session_id, id) on delete restrict
);
create index session_messages_message_idx on session_messages (project_id, message_id);
create index session_messages_run_idx on session_messages (run_id, revision) where run_id is not null;
alter table runs add constraint runs_final_message_membership_fk
    foreign key (project_id, session_id, id, final_message_id)
    references session_messages(project_id, session_id, run_id, message_id)
    on delete restrict deferrable initially deferred;

create table run_checkpoints (
    run_id uuid primary key references runs(id) on delete restrict,
    version bigint not null default 1 check (version > 0),
    state jsonb not null check (jsonb_typeof(state) = 'object'),
    saved_by_lease_epoch bigint not null check (saved_by_lease_epoch > 0),
    updated_at timestamptz not null default now()
);

create table run_inputs (
    id uuid primary key,
    project_id uuid not null,
    run_id uuid not null,
    sequence bigint not null check (sequence > 0),
    kind text not null check (kind = btrim(kind) and char_length(kind) between 1 and 128),
    source_run_id uuid,
    deduplication_key text not null check (deduplication_key = btrim(deduplication_key) and char_length(deduplication_key) between 1 and 256),
    payload jsonb not null check (jsonb_typeof(payload) = 'object'),
    status text not null default 'pending' check (status in ('pending', 'handled', 'rejected')),
    handling jsonb check (jsonb_typeof(handling) = 'object'),
    created_at timestamptz not null default now(),
    handled_at timestamptz check (handled_at >= created_at),
    unique (run_id, sequence),
    unique (run_id, deduplication_key),
    foreign key (project_id, run_id) references runs(project_id, id) on delete restrict,
    foreign key (project_id, source_run_id) references runs(project_id, id) on delete restrict,
    check ((status = 'pending') = (handled_at is null)),
    check (status <> 'pending' or handling is null)
);
create index run_inputs_pending_idx on run_inputs (run_id, sequence) where status = 'pending';
create index run_inputs_source_idx on run_inputs (project_id, source_run_id) where source_run_id is not null;

create table run_waits (
    id uuid primary key,
    project_id uuid not null,
    run_id uuid not null,
    wait_key text not null check (wait_key = btrim(wait_key) and char_length(wait_key) between 1 and 256),
    mode text not null check (mode in ('any', 'all')),
    status text not null default 'pending' check (status in ('pending', 'satisfied', 'cancelled', 'timed_out')),
    deadline_at timestamptz,
    metadata jsonb not null default '{}' check (jsonb_typeof(metadata) = 'object'),
    result jsonb check (jsonb_typeof(result) = 'object'),
    created_at timestamptz not null default now(),
    resolved_at timestamptz check (resolved_at >= created_at),
    unique (project_id, id),
    unique (run_id, wait_key),
    foreign key (project_id, run_id) references runs(project_id, id) on delete restrict,
    check ((status = 'pending') = (resolved_at is null)),
    check (status <> 'pending' or result is null),
    check (status <> 'timed_out' or (deadline_at is not null and resolved_at >= deadline_at))
);
create index run_waits_deadline_idx on run_waits (deadline_at, id) where status = 'pending' and deadline_at is not null;
create index run_waits_project_idx on run_waits (project_id, run_id);

create table run_wait_dependencies (
    id uuid primary key,
    project_id uuid not null,
    wait_id uuid not null,
    kind text not null check (kind in ('run_completion', 'input', 'timer', 'operation')),
    target_run_id uuid,
    input_kind text check (input_kind = btrim(input_kind) and char_length(input_kind) between 1 and 128),
    correlation_key text check (correlation_key = btrim(correlation_key) and char_length(correlation_key) between 1 and 256),
    wake_at timestamptz,
    satisfied_at timestamptz,
    result jsonb check (jsonb_typeof(result) = 'object'),
    foreign key (project_id, wait_id) references run_waits(project_id, id) on delete restrict,
    foreign key (project_id, target_run_id) references runs(project_id, id) on delete restrict,
    constraint run_wait_dependencies_target_valid check (
        (kind = 'run_completion' and target_run_id is not null and input_kind is null and correlation_key is null and wake_at is null) or
        (kind = 'input' and target_run_id is null and input_kind is not null and wake_at is null) or
        (kind = 'timer' and target_run_id is null and input_kind is null and correlation_key is null and wake_at is not null) or
        (kind = 'operation' and target_run_id is null and input_kind is null and correlation_key is not null and wake_at is null)
    ),
    check (satisfied_at is not null or result is null),
    check (kind <> 'timer' or satisfied_at is null or satisfied_at >= wake_at)
);
create index run_wait_dependencies_wait_idx on run_wait_dependencies (project_id, wait_id);
create unique index run_wait_dependencies_run_unique on run_wait_dependencies (wait_id, target_run_id) where kind = 'run_completion';
create unique index run_wait_dependencies_input_unique on run_wait_dependencies (wait_id, input_kind, coalesce(correlation_key, '')) where kind = 'input';
create unique index run_wait_dependencies_timer_unique on run_wait_dependencies (wait_id, wake_at) where kind = 'timer';
create unique index run_wait_dependencies_operation_unique on run_wait_dependencies (wait_id, correlation_key) where kind = 'operation';
create index run_wait_dependencies_target_idx on run_wait_dependencies (project_id, target_run_id) where target_run_id is not null;
create index run_wait_dependencies_due_idx on run_wait_dependencies (wake_at, id) where kind = 'timer' and satisfied_at is null;
create index run_wait_dependencies_correlation_idx on run_wait_dependencies (correlation_key, wait_id) where correlation_key is not null and satisfied_at is null;

create table run_events (
    id uuid primary key,
    run_id uuid not null references runs(id) on delete restrict,
    sequence bigint not null check (sequence > 0),
    type text not null check (type = btrim(type) and char_length(type) between 1 and 128),
    source text not null check (source in ('runtime', 'harness')),
    payload jsonb not null default '{}' check (jsonb_typeof(payload) = 'object'),
    occurred_at timestamptz not null default now(),
    recorded_at timestamptz not null default now(),
    unique (run_id, sequence),
    check (type not in ('run.started', 'run.waiting', 'run.resumed', 'run.completed', 'run.failed', 'run.abort_requested', 'run.aborted') or source = 'runtime')
);

create table runtime_requests (
    project_id uuid not null references projects(project_id) on delete restrict,
    scope_kind text not null check (scope_kind ~ '^(project|session|run)\.[a-z][a-z0-9_.]{0,127}$'),
    scope_id uuid not null,
    key text not null check (key = btrim(key) and char_length(key) between 1 and 256),
    request_hash text not null check (request_hash ~ '^[a-f0-9]{64}$'),
    result jsonb not null check (jsonb_typeof(result) = 'object'),
    created_at timestamptz not null default now(),
    expires_at timestamptz check (expires_at > created_at),
    primary key (scope_kind, scope_id, key),
    -- Run-scoped receipts survive arbitrarily long waits/recovery. Retention
    -- after permanent archival is a future explicit policy.
    check (split_part(scope_kind, '.', 1) <> 'run' or expires_at is null)
);
create index runtime_requests_project_idx on runtime_requests (project_id);
create index runtime_requests_expiry_idx on runtime_requests (expires_at) where expires_at is not null;

-- Append-only records and identity fields must remain stable even for writers
-- added after the initial runtime implementation. No cascading history deletes.
create function runtime_reject_change() returns trigger language plpgsql as $$
begin
    raise exception '% is append-only; % is not supported', tg_table_name, tg_op
        using errcode = '23514', constraint = 'runtime_append_only';
end;
$$;
create trigger messages_append_only before update or delete on messages for each row execute function runtime_reject_change();
create trigger session_messages_append_only before update or delete on session_messages for each row execute function runtime_reject_change();
create trigger run_events_append_only before update or delete on run_events for each row execute function runtime_reject_change();
create trigger sessions_no_delete before delete on sessions for each row execute function runtime_reject_change();
create trigger runs_no_delete before delete on runs for each row execute function runtime_reject_change();
create trigger run_checkpoints_no_delete before delete on run_checkpoints for each row execute function runtime_reject_change();
create trigger run_inputs_no_delete before delete on run_inputs for each row execute function runtime_reject_change();
create trigger run_waits_no_delete before delete on run_waits for each row execute function runtime_reject_change();
create trigger run_wait_dependencies_no_delete before delete on run_wait_dependencies for each row execute function runtime_reject_change();
create trigger runtime_requests_no_update before update on runtime_requests for each row execute function runtime_reject_change();

create function runtime_guard_session() returns trigger language plpgsql as $$
declare parent_revision bigint;
begin
    if tg_op = 'INSERT' then
        if new.current_revision <> 0 then
            raise exception 'new sessions start at revision zero' using errcode = '23514';
        end if;
        if new.forked_from_session_id is not null then
            select current_revision into parent_revision from sessions
                where id = new.forked_from_session_id and project_id = new.project_id for key share;
            if parent_revision is null or new.forked_at_revision > parent_revision then
                raise exception 'fork cutoff must exist in the source session' using errcode = '23514';
            end if;
        end if;
    else
        if (new.id, new.project_id, new.harness_id, new.forked_from_session_id, new.forked_at_revision, new.created_at)
            is distinct from (old.id, old.project_id, old.harness_id, old.forked_from_session_id, old.forked_at_revision, old.created_at)
            or new.current_revision < old.current_revision or new.last_activity_at < old.last_activity_at then
            raise exception 'session identity, ancestry and history cannot be rewritten' using errcode = '23514';
        end if;
    end if;
    return new;
end;
$$;
create trigger sessions_guard before insert or update on sessions for each row execute function runtime_guard_session();

create function runtime_append_session_message() returns trigger language plpgsql as $$
declare s sessions%rowtype; origin uuid; inherited uuid;
begin
    select * into s from sessions where id = new.session_id and project_id = new.project_id for update;
    if not found then raise exception 'session not found in project' using errcode = '23503'; end if;
    if new.revision is null then new.revision := s.current_revision + 1; end if;
    if new.revision <> s.current_revision + 1 then
        raise exception 'messages must append at the next session revision' using errcode = '23514';
    end if;
    select origin_run_id into origin from messages where id = new.message_id and project_id = new.project_id;
    if not found then raise exception 'message not found in project' using errcode = '23503'; end if;
    if s.forked_from_session_id is not null and new.revision <= s.forked_at_revision then
        select message_id into inherited from session_messages
            where session_id = s.forked_from_session_id and revision = new.revision;
        if inherited is distinct from new.message_id or new.run_id is not null then
            raise exception 'fork must inherit the exact source prefix without child run attribution' using errcode = '23514';
        end if;
    elsif origin is distinct from new.run_id then
        raise exception 'local history must retain its originating run attribution' using errcode = '23514';
    end if;
    update sessions set current_revision = new.revision,
        last_activity_at = greatest(last_activity_at, clock_timestamp()) where id = s.id;
    return new;
end;
$$;
create trigger session_messages_append before insert on session_messages for each row execute function runtime_append_session_message();

create function runtime_check_session_history() returns trigger language plpgsql as $$
declare s sessions%rowtype; actual bigint;
begin
    select * into s from sessions where id = new.id;
    select coalesce(max(revision), 0) into actual from session_messages where session_id = s.id;
    if s.current_revision <> actual or s.current_revision < coalesce(s.forked_at_revision, 0) then
        raise exception 'session revision and complete fork prefix must commit with history' using errcode = '23514';
    end if;
    return null;
end;
$$;
create constraint trigger sessions_history_consistent after insert or update on sessions
    deferrable initially deferred for each row execute function runtime_check_session_history();

create function runtime_guard_run() returns trigger language plpgsql as $$
declare selected_harness text;
begin
    if tg_op = 'INSERT' then
        if new.status <> 'ready' or new.version <> 1 or new.lease_epoch <> 0 or new.last_event_sequence <> 0 then
            raise exception 'new runs must start ready without execution ownership' using errcode = '23514';
        end if;
        perform 1 from sessions s join harnesses h on h.id = s.harness_id
            where s.id = new.session_id and s.project_id = new.project_id and h.enabled for share of h;
        if not found then raise exception 'new run requires an enabled harness in its project' using errcode = '23514'; end if;
        if new.parent_run_id is not null then
            -- Parents must already exist. Together with immutable parent IDs,
            -- this also rules out cycles introduced by multi-row inserts.
            perform 1 from runs where id = new.parent_run_id and project_id = new.project_id for key share;
            if not found then raise exception 'parent run not found in project' using errcode = '23503'; end if;
        end if;
    else
        if (new.id, new.project_id, new.session_id, new.parent_run_id, new.config, new.created_at)
            is distinct from (old.id, old.project_id, old.session_id, old.parent_run_id, old.config, old.created_at)
            or new.version < old.version or new.lease_epoch < old.lease_epoch or new.last_event_sequence < old.last_event_sequence
            or (old.started_at is not null and new.started_at is distinct from old.started_at)
            or (old.abort_requested_at is not null and new.abort_requested_at is distinct from old.abort_requested_at) then
            raise exception 'run identity/configuration and monotonic state cannot be rewritten' using errcode = '23514';
        end if;
        if old.status in ('completed', 'failed', 'aborted')
            and (to_jsonb(new) - 'last_event_sequence') is distinct from (to_jsonb(old) - 'last_event_sequence') then
            raise exception 'terminal runs cannot be reopened or changed' using errcode = '23514';
        end if;
        if (new.status, new.available_at, new.worker_id, new.lease_epoch, new.abort_requested_at)
            is distinct from (old.status, old.available_at, old.worker_id, old.lease_epoch, old.abort_requested_at)
            and new.version <> old.version + 1 then
            raise exception 'run transitions require the next version' using errcode = '23514';
        end if;
        if new.status = 'running' and (old.status <> 'running' or new.worker_id is distinct from old.worker_id or new.lease_epoch <> old.lease_epoch) then
            if old.status not in ('ready', 'running')
                or (old.status = 'ready' and old.available_at > clock_timestamp())
                or (old.status = 'running' and old.lease_expires_at > clock_timestamp())
                or new.lease_expires_at <= clock_timestamp() then
                raise exception 'run is not eligible for new execution ownership' using errcode = '23514';
            end if;
            if new.lease_epoch <> old.lease_epoch + 1 then
                raise exception 'new execution ownership requires the next lease epoch' using errcode = '23514';
            end if;
            select harness_id into selected_harness from sessions where id = new.session_id;
            perform 1 from workers where id = new.worker_id and status = 'accepting'
                and selected_harness = any(supported_harnesses) for share;
            if not found then raise exception 'worker cannot accept this harness' using errcode = '23514'; end if;
        elsif new.lease_epoch <> old.lease_epoch then
            raise exception 'lease epochs change only when acquiring execution ownership' using errcode = '23514';
        end if;
    end if;
    return new;
end;
$$;
create trigger runs_guard before insert or update on runs for each row execute function runtime_guard_run();

create function runtime_check_run_completion() returns trigger language plpgsql as $$
declare r runs%rowtype; body jsonb; event_type text;
begin
    select * into r from runs where id = new.id;
    if r.status = 'completed' then
        select message into body from messages where id = r.final_message_id;
        if body is null or body->>'role' <> 'assistant'
            or body->>'stop_reason' = 'tool_use'
            or jsonb_path_exists(body, '$.content[*] ? (@.type == "tool_call")') then
            raise exception 'completion requires a final assistant message without tool calls' using errcode = '23514';
        end if;
    end if;
    if r.status in ('completed', 'failed', 'aborted') then
        event_type := 'run.' || r.status;
        if not exists (select 1 from run_events where run_id = r.id and type = event_type and source = 'runtime') then
            raise exception 'terminal transition must commit with its lifecycle event' using errcode = '23514';
        end if;
        if exists (select 1 from run_waits where run_id = r.id and status = 'pending') then
            raise exception 'terminal runs cannot retain pending waits' using errcode = '23514';
        end if;
    end if;
    if r.status = 'waiting' and not exists (select 1 from run_waits where run_id = r.id and status = 'pending') then
        raise exception 'suspended runs require a pending durable wait' using errcode = '23514';
    end if;
    if r.last_event_sequence <> (select coalesce(max(sequence), 0) from run_events where run_id = r.id) then
        raise exception 'run event counter must commit with its event' using errcode = '23514';
    end if;
    if exists (select 1 from run_events where run_id = r.id
        and type in ('run.completed', 'run.failed', 'run.aborted') and type <> 'run.' || r.status) then
        raise exception 'terminal lifecycle event must agree with run status' using errcode = '23514';
    end if;
    return null;
end;
$$;
create constraint trigger runs_completion_consistent after insert or update on runs
    deferrable initially deferred for each row execute function runtime_check_run_completion();

create function runtime_guard_checkpoint() returns trigger language plpgsql as $$
declare r runs%rowtype;
begin
    select * into r from runs where id = new.run_id for update;
    if not found then raise exception 'run not found' using errcode = '23503'; end if;
    if r.status <> 'running' or new.saved_by_lease_epoch <> r.lease_epoch or r.lease_expires_at <= clock_timestamp() then
        raise exception 'checkpoint requires current, unexpired execution ownership' using errcode = '23514';
    end if;
    if (tg_op = 'INSERT' and new.version <> 1) or
        (tg_op = 'UPDATE' and (new.run_id <> old.run_id or new.version <> old.version + 1)) then
        raise exception 'checkpoint writes require the next version' using errcode = '23514';
    end if;
    new.updated_at := clock_timestamp();
    return new;
end;
$$;
create trigger run_checkpoints_guard before insert or update on run_checkpoints for each row execute function runtime_guard_checkpoint();

create function runtime_guard_input() returns trigger language plpgsql as $$
declare current_status text; next_sequence bigint;
begin
    if tg_op = 'INSERT' then
        select status into current_status from runs where id = new.run_id for update;
        if current_status in ('completed', 'failed', 'aborted') then
            raise exception 'terminal runs cannot accept new input' using errcode = '23514';
        end if;
        if new.status <> 'pending' then raise exception 'new inputs start pending' using errcode = '23514'; end if;
        select coalesce(max(sequence), 0) + 1 into next_sequence from run_inputs where run_id = new.run_id;
        if new.sequence is null then new.sequence := next_sequence; end if;
        if new.sequence <> next_sequence then raise exception 'input sequence must append' using errcode = '23514'; end if;
    elsif (new.id, new.project_id, new.run_id, new.sequence, new.kind, new.source_run_id, new.deduplication_key, new.payload, new.created_at)
        is distinct from (old.id, old.project_id, old.run_id, old.sequence, old.kind, old.source_run_id, old.deduplication_key, old.payload, old.created_at)
        or old.status <> 'pending' then
        raise exception 'input identity/payload and settled handling are immutable' using errcode = '23514';
    end if;
    return new;
end;
$$;
create trigger run_inputs_guard before insert or update on run_inputs for each row execute function runtime_guard_input();

create function runtime_guard_wait() returns trigger language plpgsql as $$
declare current_status text;
begin
    if tg_op = 'INSERT' then
        select status into current_status from runs where id = new.run_id for update;
        if current_status in ('completed', 'failed', 'aborted') or new.status <> 'pending' then
            raise exception 'new waits require a live run and start pending' using errcode = '23514';
        end if;
    elsif (new.id, new.project_id, new.run_id, new.wait_key, new.mode, new.deadline_at, new.metadata, new.created_at)
        is distinct from (old.id, old.project_id, old.run_id, old.wait_key, old.mode, old.deadline_at, old.metadata, old.created_at)
        or old.status <> 'pending' then
        raise exception 'wait identity/conditions and resolved results are immutable' using errcode = '23514';
    end if;
    return new;
end;
$$;
create trigger run_waits_guard before insert or update on run_waits for each row execute function runtime_guard_wait();

create function runtime_guard_dependency() returns trigger language plpgsql as $$
declare waiting_run uuid; wait_status text;
begin
    select run_id, status into waiting_run, wait_status from run_waits where id = new.wait_id for update;
    if tg_op = 'INSERT' then
        if wait_status <> 'pending' or new.target_run_id = waiting_run then
            raise exception 'dependencies require an open wait and cannot target their own run' using errcode = '23514';
        end if;
    elsif (new.id, new.project_id, new.wait_id, new.kind, new.target_run_id, new.input_kind, new.correlation_key, new.wake_at)
        is distinct from (old.id, old.project_id, old.wait_id, old.kind, old.target_run_id, old.input_kind, old.correlation_key, old.wake_at)
        or old.satisfied_at is not null then
        raise exception 'dependency identity and satisfied results are immutable' using errcode = '23514';
    end if;
    return new;
end;
$$;
create trigger run_wait_dependencies_guard before insert or update on run_wait_dependencies for each row execute function runtime_guard_dependency();

create function runtime_check_wait() returns trigger language plpgsql as $$
declare w run_waits%rowtype; total bigint; satisfied bigint; current_status text;
begin
    if tg_table_name = 'run_waits' then
        select * into w from run_waits where id = new.id;
    else
        select * into w from run_waits where id = new.wait_id;
    end if;
    select count(*), count(satisfied_at) into total, satisfied from run_wait_dependencies where wait_id = w.id;
    if total = 0 then raise exception 'a durable wait requires at least one dependency' using errcode = '23514'; end if;
    if w.status = 'satisfied' and ((w.mode = 'all' and total <> satisfied) or (w.mode = 'any' and satisfied = 0)) then
        raise exception 'wait condition is not satisfied' using errcode = '23514';
    end if;
    if exists (select 1 from run_wait_dependencies d join runs target on target.id = d.target_run_id
        where d.wait_id = w.id and d.kind = 'run_completion' and d.satisfied_at is not null
        and target.status not in ('completed', 'failed', 'aborted')) then
        raise exception 'run completion dependency requires a terminal target' using errcode = '23514';
    end if;
    select status into current_status from runs where id = w.run_id;
    if current_status in ('completed', 'failed', 'aborted') and w.status = 'pending' then
        raise exception 'terminal runs cannot retain pending waits' using errcode = '23514';
    end if;
    if current_status = 'waiting' and not exists (select 1 from run_waits where run_id = w.run_id and status = 'pending') then
        raise exception 'resolving the last wait must also wake its suspended run' using errcode = '23514';
    end if;
    return null;
end;
$$;
create constraint trigger run_waits_consistent after insert or update on run_waits
    deferrable initially deferred for each row execute function runtime_check_wait();
create constraint trigger run_wait_dependencies_consistent after insert or update on run_wait_dependencies
    deferrable initially deferred for each row execute function runtime_check_wait();

create function runtime_append_event() returns trigger language plpgsql as $$
declare next_sequence bigint;
begin
    update runs set last_event_sequence = last_event_sequence + 1 where id = new.run_id returning last_event_sequence into next_sequence;
    if not found then raise exception 'run not found' using errcode = '23503'; end if;
    if new.sequence is null then new.sequence := next_sequence; end if;
    if new.sequence <> next_sequence then raise exception 'event sequence must append' using errcode = '23514'; end if;
    return new;
end;
$$;
create trigger run_events_append before insert on run_events for each row execute function runtime_append_event();

create function runtime_check_request_scope() returns trigger language plpgsql as $$
begin
    case split_part(new.scope_kind, '.', 1)
        when 'project' then
            if new.scope_id <> new.project_id then raise exception 'request scope project mismatch' using errcode = '23503'; end if;
        when 'session' then
            perform 1 from sessions where id = new.scope_id and project_id = new.project_id for key share;
            if not found then raise exception 'request scope session not found in project' using errcode = '23503'; end if;
        when 'run' then
            perform 1 from runs where id = new.scope_id and project_id = new.project_id for key share;
            if not found then raise exception 'request scope run not found in project' using errcode = '23503'; end if;
        else raise exception 'unsupported request scope' using errcode = '23514';
    end case;
    return new;
end;
$$;
create trigger runtime_requests_scope before insert on runtime_requests for each row execute function runtime_check_request_scope();

create function runtime_guard_request_delete() returns trigger language plpgsql as $$
begin
    if old.expires_at is null or old.expires_at > clock_timestamp() then
        raise exception 'only expired coordination receipts may be removed' using errcode = '23514';
    end if;
    return old;
end;
$$;
create trigger runtime_requests_retention before delete on runtime_requests for each row execute function runtime_guard_request_delete();

comment on table run_checkpoints is 'Latest harness-owned recovery state; not an arbitrary suspended Rust stack.';
comment on table runtime_requests is 'Commit receipts in the same transaction as their mutations; hashes are checked by the runtime interface on replay.';
comment on table workers is 'Operational metadata only; per-run leases determine accepted execution ownership.';
comment on table run_inputs is 'Persisted delivery is distinct from model application; handling/checkpoint commits must be coordinated by the runtime interface.';
