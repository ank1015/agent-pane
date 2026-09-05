-- Catalog availability is independent of a worker build's advertised capability.
-- Preserve any deliberately preconfigured row with this ID.
insert into harnesses(id, name, description, default_config, config_schema, enabled)
values (
  'environments',
  'Environments',
  'Prepare machine and snapshot-backed sandbox environments without compaction',
  '{"reasoning_level":"high","web_search_enabled":true}'::jsonb,
  '{
    "type":"object", "additionalProperties":false,
    "required":["model","reasoning_level"],
    "properties":{
      "model":{"type":"object","additionalProperties":false,"required":["provider","id"],"properties":{
        "provider":{"enum":["openai","chatgpt","fireworks"]},
        "id":{"type":"string","minLength":1},"name":{"type":["string","null"]}}},
      "reasoning_level":{"enum":["low","medium","high","xhigh","max"]},
      "account_id":{"type":["string","null"],"format":"uuid"},
      "web_search_enabled":{"type":"boolean","default":true},
      "system_prompt_append":{"type":["string","null"],"maxLength":8192}
    }
  }'::jsonb,
  true
)
on conflict (id) do nothing;
