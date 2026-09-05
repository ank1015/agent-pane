alter table e2b_hosts
add column ram_mb integer;

update e2b_hosts
set ram_mb = 2048
where source_type = 'base';

alter table e2b_hosts
add constraint e2b_hosts_ram_matches_source check (
    (source_type = 'base' and ram_mb in (1024, 2048, 4096, 8192))
    or (source_type = 'snapshot' and ram_mb is null)
);
