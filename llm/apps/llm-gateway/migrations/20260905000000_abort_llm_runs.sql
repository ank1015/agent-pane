alter table llm_runs drop constraint llm_runs_status_check;
alter table llm_runs add constraint llm_runs_status_check
    check (status in ('running', 'succeeded', 'failed', 'aborted', 'expired'));
