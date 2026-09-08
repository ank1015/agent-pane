-- Existing sessions retain their frozen declarations; new basic sessions expose
-- the resolved target for each run, independently of private harness state.
update harnesses set harness_contract=jsonb_set(harness_contract,'{outputs}',
    coalesce(harness_contract->'outputs','{}'::jsonb)||'{"workspace":{"kind":"execution_workspace","description":"Actual workspace used by this run; inspect run status before verification."}}'::jsonb),
    updated_at=clock_timestamp()
where id in ('basic-cc-tools-harness','basic-codex-tools-harness');
