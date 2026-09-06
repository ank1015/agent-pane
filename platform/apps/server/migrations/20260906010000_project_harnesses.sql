alter table harnesses add column project_policy text not null default 'opt_in'
    check (project_policy in ('required', 'opt_in'));

update harnesses set project_policy='required', updated_at=clock_timestamp()
where id='environments';

create table project_harnesses (
    project_id uuid not null references projects(project_id) on delete cascade,
    harness_id text not null references harnesses(id) on delete restrict,
    enabled boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now() check (updated_at >= created_at),
    primary key (project_id, harness_id)
);
create index project_harnesses_harness_idx on project_harnesses(harness_id);

-- Preserve existing workflows, not blanket access to the entire catalogue.
insert into project_harnesses(project_id,harness_id,enabled)
select distinct s.project_id,s.harness_id,true
from sessions s join harnesses h on h.id=s.harness_id
where h.project_policy='opt_in';
