-- New configuration grants are explicit. Legacy single/none adapters remain valid.
alter table site_harness_grants drop constraint site_harness_grants_environment_mode_check;
alter table site_harness_grants add constraint site_harness_grants_environment_mode_check
    check (environment_mode in ('none','single','declared'));
alter table site_harness_grants alter column environment_mode set default 'declared';
alter table site_harness_grants add column configurable_fields text[] not null default '{}'
    check (cardinality(configurable_fields) <= 64);
-- A session can now be created without starting a run.
alter table site_runtime_operations alter column run_id drop not null;
