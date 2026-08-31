-- Fixed legacy project used only to preserve environments created before project ownership existed.
-- New records must always provide their actual project ID through the control API.
alter table environments add column project_id uuid;
alter table sandbox_environment_templates add column project_id uuid;

update environments
set project_id = '00000000-0000-0000-0000-000000000001';

update sandbox_environment_templates
set project_id = '00000000-0000-0000-0000-000000000001';

alter table environments alter column project_id set not null;
alter table sandbox_environment_templates alter column project_id set not null;

create index environments_active_project_created_idx
    on environments(project_id, created_at, environment_id)
    where deleted_at is null;

create index sandbox_environment_templates_active_project_created_idx
    on sandbox_environment_templates(project_id, created_at, id)
    where deleted_at is null;
