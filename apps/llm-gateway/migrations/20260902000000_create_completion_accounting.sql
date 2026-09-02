create table llm_completion_accounting (
    request_id uuid primary key,
    account_id uuid,
    requested_provider text not null,
    requested_model text not null,
    response_provider text not null,
    response_model text not null,
    assistant_message_id text not null,
    usage jsonb,
    input_tokens numeric generated always as ((usage ->> 'input')::numeric) stored,
    output_tokens numeric generated always as ((usage ->> 'output')::numeric) stored,
    cache_read_tokens numeric generated always as ((usage ->> 'cache_read')::numeric) stored,
    cache_write_tokens numeric generated always as ((usage ->> 'cache_write')::numeric) stored,
    input_cost_usd numeric generated always as ((usage #>> '{cost,input}')::numeric) stored,
    output_cost_usd numeric generated always as ((usage #>> '{cost,output}')::numeric) stored,
    cache_read_cost_usd numeric generated always as ((usage #>> '{cost,cache_read}')::numeric) stored,
    cache_write_cost_usd numeric generated always as ((usage #>> '{cost,cache_write}')::numeric) stored,
    total_cost_usd numeric generated always as ((usage #>> '{cost,total}')::numeric) stored,
    provider_duration_ms numeric not null,
    completed_at timestamptz not null default now(),

    constraint llm_completion_accounting_account_id_fkey
        foreign key (account_id) references provider_accounts (id) on delete set null,
    constraint llm_completion_accounting_requested_provider_not_blank
        check (requested_provider = btrim(requested_provider) and requested_provider <> ''),
    constraint llm_completion_accounting_requested_model_not_blank
        check (requested_model = btrim(requested_model) and requested_model <> ''),
    constraint llm_completion_accounting_response_provider_not_blank
        check (response_provider = btrim(response_provider) and response_provider <> ''),
    constraint llm_completion_accounting_response_model_not_blank
        check (response_model = btrim(response_model) and response_model <> ''),
    constraint llm_completion_accounting_assistant_message_id_not_blank
        check (assistant_message_id = btrim(assistant_message_id) and assistant_message_id <> ''),
    constraint llm_completion_accounting_usage_is_object
        check (usage is null or jsonb_typeof(usage) = 'object'),
    constraint llm_completion_accounting_provider_duration_non_negative
        check (provider_duration_ms >= 0)
);

create index llm_completion_accounting_completed_at_idx
    on llm_completion_accounting (completed_at desc);

create index llm_completion_accounting_provider_model_idx
    on llm_completion_accounting (requested_provider, requested_model, completed_at desc);

create index llm_completion_accounting_account_idx
    on llm_completion_accounting (account_id, completed_at desc)
    where account_id is not null;
