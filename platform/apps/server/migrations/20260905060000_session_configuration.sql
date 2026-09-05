-- Freeze the most recent configuration of existing sessions; retain all historical
-- run snapshots unchanged. Empty legacy sessions inherit their harness defaults.
alter table sessions add column config jsonb;
update sessions s set config = coalesce(
    (select r.config from runs r where r.session_id=s.id order by r.created_at desc,r.id desc limit 1),
    (select h.default_config from harnesses h where h.id=s.harness_id)
);
-- Backfill updates queue the runtime's deferred history checks. Validate them
-- before further ALTER TABLE statements, including on nonempty deployments.
set constraints all immediate;
alter table sessions alter column config set not null;
alter table sessions add constraint sessions_config_object check (jsonb_typeof(config)='object');

-- Upgrade the basic harness's former direct-host config for future runs only.
update sessions set config = (config - 'execution') || jsonb_build_object('environment',
    ((config->'execution') - 'host_id') || jsonb_build_object('type','machine','machine_id',config->'execution'->'host_id'))
where harness_id='basic-cc-tools-harness' and config ? 'execution' and not config ? 'environment';

create function runtime_guard_session_config() returns trigger language plpgsql as $$
begin
    if new.config is distinct from old.config then
        raise exception 'session configuration is immutable; fork to change it' using errcode = '23514';
    end if;
    return new;
end;
$$;
create trigger sessions_config_immutable before update of config on sessions
    for each row execute function runtime_guard_session_config();

create function runtime_snapshot_session_config() returns trigger language plpgsql as $$
begin
    if new.config is distinct from (select config from sessions where id=new.session_id) then
        raise exception 'new runs must use their session configuration' using errcode = '23514';
    end if;
    return new;
end;
$$;
create trigger runs_session_config before insert on runs
    for each row execute function runtime_snapshot_session_config();
