alter table snapshots add column name text;

update snapshots set name = provider_snapshot_id;

alter table snapshots
    alter column name set not null,
    add constraint snapshots_name_not_blank
        check (name = btrim(name) and name <> '');
