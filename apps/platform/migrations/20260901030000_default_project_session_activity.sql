alter table project_sessions
    alter column last_activity_at set default now();
