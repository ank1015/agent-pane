alter table environments
    add column name text;

update environments
set name = path;

alter table environments
    alter column name set not null,
    add constraint environments_name_not_empty check (btrim(name) <> '');
