-- Sites is a Platform-owned harness and is available to every project while
-- globally enabled. Project-specific opt-in rows are no longer meaningful.
update harnesses
set project_policy = 'required', updated_at = clock_timestamp()
where id = 'sites' and project_policy <> 'required';

delete from project_harnesses where harness_id = 'sites';
