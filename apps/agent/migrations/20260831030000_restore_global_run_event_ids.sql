alter table run_events drop constraint run_events_run_event_id_unique;
alter table run_events add constraint run_events_event_id_unique unique (event_id);
