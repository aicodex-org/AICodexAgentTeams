-- 请求历史保存模型映射的不可变快照，兼容没有映射的旧记录
create or replace function model_request_observation_valid(document jsonb) returns boolean
language plpgsql immutable parallel safe as $$
declare
  spec record;
  item jsonb;
  number numeric;
  mapping jsonb;
  policy jsonb;
begin
  if document is null or jsonb_typeof(document) <> 'object'
     or octet_length(document::text) > 1048576 then
    return false;
  end if;
  for spec in select * from (values
    ('{}'::text[], array['account','continuation','error','recovery','request','response','routing','scheduling','timings','transport']::text[]),
    ('{request}'::text[], array['clientApiKeyNameSnapshot','clientIp','compact','configRevision','endpoint','protocol','reasoningEffort','reasoningPreset','subagentKind','userAgent']::text[]),
    ('{account}'::text[], array['authenticationKind','email','name','planTypeSnapshot','notesSnapshot']::text[]),
    ('{routing}'::text[], array['groupNamesSnapshot','groupRefs','scope','modelMapping']::text[]),
    ('{error}'::text[], array['message','providerErrorCode','retryAfterMs']::text[]),
    ('{timings}'::text[], array['local','upstream']::text[]),
    ('{timings,local}'::text[], array['connectMs','firstEventMs','firstReasoningMs','firstTextMs','firstTokenMs','headersMs','latencyMs','transportDecisionWaitMs']::text[]),
    ('{timings,upstream}'::text[], array['processingMs','responseMs','apiOverheadMs','engineMs','engineIapiTtftMs','engineServiceTtftMs','engineIapiTbtMs','engineServiceTbtMs']::text[]),
    ('{scheduling}'::text[], array['accountSelectionWaitMs','admissionDecisionMs','capacityTotalSlots','capacityUsedSlots']::text[]),
    ('{transport}'::text[], array['connection','httpVersion','websocketPool']::text[]),
    ('{transport,connection}'::text[], array['ageMs','exitReason','id','idleMs']::text[]),
    ('{response}'::text[], array['responseModel']::text[]),
    ('{continuation}'::text[], array['previousResponseIdHash','unavailableReason']::text[]),
    ('{recovery}'::text[], array['retryDelayMs','totalLatencyMs']::text[])) as groups(path, allowed_keys) loop
    item := document #> spec.path;
    if item is not null and (jsonb_typeof(item) <> 'object' or item - spec.allowed_keys <> '{}'::jsonb) then
      return false;
    end if;
  end loop;
  for spec in select * from (values
    ('{request,configRevision}'::text[], 'number'),
    ('{request,protocol}'::text[], 'string'),
    ('{request,endpoint}'::text[], 'string'),
    ('{request,clientApiKeyNameSnapshot}'::text[], 'string'),
    ('{account,planTypeSnapshot}'::text[], 'string'),
    ('{account,notesSnapshot}'::text[], 'string'),
    ('{account,name}'::text[], 'string'),
    ('{account,email}'::text[], 'string'),
    ('{account,authenticationKind}'::text[], 'string'),
    ('{routing,scope}'::text[], 'string'),
    ('{routing,groupRefs}'::text[], 'array'),
    ('{routing,groupNamesSnapshot}'::text[], 'array'),
    ('{error,providerErrorCode}'::text[], 'string'),
    ('{error,message}'::text[], 'string'),
    ('{error,retryAfterMs}'::text[], 'number'),
    ('{request,clientIp}'::text[], 'string'),
    ('{request,userAgent}'::text[], 'string'),
    ('{request,reasoningEffort}'::text[], 'string'),
    ('{request,reasoningPreset}'::text[], 'string'),
    ('{request,subagentKind}'::text[], 'string'),
    ('{request,compact}'::text[], 'boolean'),
    ('{timings,local,transportDecisionWaitMs}'::text[], 'number'),
    ('{timings,local,connectMs}'::text[], 'number'),
    ('{timings,local,headersMs}'::text[], 'number'),
    ('{timings,local,firstEventMs}'::text[], 'number'),
    ('{timings,local,firstReasoningMs}'::text[], 'number'),
    ('{timings,local,firstTextMs}'::text[], 'number'),
    ('{timings,local,firstTokenMs}'::text[], 'number'),
    ('{timings,local,latencyMs}'::text[], 'number'),
    ('{timings,upstream,processingMs}'::text[], 'number'),
    ('{timings,upstream,apiOverheadMs}'::text[], 'milliseconds'),
    ('{timings,upstream,engineMs}'::text[], 'milliseconds'),
    ('{timings,upstream,engineIapiTtftMs}'::text[], 'milliseconds'),
    ('{timings,upstream,engineServiceTtftMs}'::text[], 'milliseconds'),
    ('{timings,upstream,engineIapiTbtMs}'::text[], 'milliseconds'),
    ('{timings,upstream,engineServiceTbtMs}'::text[], 'milliseconds'),
    ('{scheduling,admissionDecisionMs}'::text[], 'number'),
    ('{scheduling,accountSelectionWaitMs}'::text[], 'number'),
    ('{scheduling,capacityUsedSlots}'::text[], 'number'),
    ('{scheduling,capacityTotalSlots}'::text[], 'number'),
    ('{transport,httpVersion}'::text[], 'string'),
    ('{transport,websocketPool}'::text[], 'string'),
    ('{transport,connection,id}'::text[], 'string'),
    ('{transport,connection,exitReason}'::text[], 'string'),
    ('{transport,connection,ageMs}'::text[], 'number'),
    ('{transport,connection,idleMs}'::text[], 'number'),
    ('{response,responseModel}'::text[], 'string'),
    ('{continuation,previousResponseIdHash}'::text[], 'string'),
    ('{continuation,unavailableReason}'::text[], 'string'),
    ('{recovery,retryDelayMs}'::text[], 'number'),
    ('{recovery,totalLatencyMs}'::text[], 'number'),
    ('{timings,upstream,responseMs}'::text[], 'number')) as fields(path, kind) loop
    item := document #> spec.path;
    if item is null or item = 'null'::jsonb then continue; end if;
    if spec.kind = 'milliseconds' then
      if jsonb_typeof(item) <> 'number' then return false; end if;
      number := (item #>> '{}')::numeric;
      if number < 0 or number > 9223372036854775807 then return false; end if;
      continue;
    end if;
    if jsonb_typeof(item) <> spec.kind then return false; end if;
    if spec.kind = 'number' then
      number := (item #>> '{}')::numeric;
      if item #>> '{}' !~ '^[0-9]+$' or number > 9223372036854775807 then return false; end if;
    elsif spec.kind = 'array' then
      if exists (select 1 from jsonb_array_elements(item) member where jsonb_typeof(member) <> 'string') then return false; end if;
    end if;
  end loop;
  if (document #> '{request,configRevision}' is not null
          and document #> '{request,configRevision}' <> 'null'::jsonb
          and document #> '{request,compact}' in ('true'::jsonb, 'false'::jsonb)
          and coalesce(length(document #>> '{request,protocol}'), 0) > 0
          and coalesce(length(document #>> '{request,endpoint}'), 0) > 0
          and jsonb_typeof(document #> '{routing,scope}') = 'string'
          and jsonb_typeof(document #> '{routing,groupRefs}') = 'array'
          and jsonb_typeof(document #> '{routing,groupNamesSnapshot}') = 'array') is not true then
    return false;
  end if;
  if document #>> '{request,clientIp}' is not null then
    perform (document #>> '{request,clientIp}')::inet;
  end if;
  mapping := document #> '{routing,modelMapping}';
  if mapping is not null then
    if jsonb_typeof(mapping) <> 'object'
       or mapping - array['source','protocol','requestedModel','routeModel','targetProvider','policies']::text[] <> '{}'::jsonb
       or (mapping->>'source' in ('group','global','extension')) is not true
       or (mapping->>'protocol' in ('openai','openai-chat','anthropic')) is not true
       or mapping->>'protocol' is distinct from document #>> '{request,protocol}'
       or jsonb_typeof(mapping->'policies') is distinct from 'array' then return false; end if;
    for item in select mapping->key from unnest(array['protocol','requestedModel','routeModel']) key loop
      if jsonb_typeof(item) is distinct from 'string' or length(item #>> '{}') = 0 then return false; end if;
    end loop;
    if mapping->>'source' <> 'global' and (jsonb_typeof(mapping->'targetProvider') is distinct from 'string' or coalesce(length(mapping->>'targetProvider'), 0) = 0) then return false; end if;
    if mapping->>'source' = 'global' and mapping->'targetProvider' is distinct from 'null'::jsonb then return false; end if;
    if mapping->>'source' = 'group' and jsonb_array_length(mapping->'policies') = 0 then return false; end if;
    if mapping->>'source' <> 'group' and jsonb_array_length(mapping->'policies') <> 0 then return false; end if;
    for policy in select * from jsonb_array_elements(mapping->'policies') loop
      if jsonb_typeof(policy) <> 'object'
         or policy - array['policyId','policyRevision','groupId','groupName']::text[] <> '{}'::jsonb then return false; end if;
      for item in select policy->key from unnest(array['policyId','groupId','groupName']) key loop
        if jsonb_typeof(item) is distinct from 'string' or length(item #>> '{}') = 0 then return false; end if;
      end loop;
      if jsonb_typeof(policy->'policyRevision') is distinct from 'number'
         or (policy->>'policyRevision' ~ '^[1-9][0-9]*$') is not true
         or (policy->>'policyRevision')::numeric > 9223372036854775807 then return false; end if;
    end loop;
  end if;
  return true;
exception when invalid_text_representation or numeric_value_out_of_range then
  return false;
end;
$$;
