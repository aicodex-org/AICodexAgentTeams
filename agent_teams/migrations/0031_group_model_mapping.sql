-- 每组最多一条策略，协议与请求模型共同确定唯一映射
create table group_model_mapping_policies (
    id text primary key,
    group_id text not null unique references account_groups(id) on delete cascade,
    enabled boolean not null default true,
    remark text,
    revision bigint not null default 1 check (revision > 0),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table group_model_mapping_entries (
    policy_id text not null references group_model_mapping_policies(id) on delete cascade,
    protocol text not null check (protocol in ('openai', 'openai-chat', 'anthropic')),
    requested_model text not null check (length(requested_model) between 1 and 256 and requested_model = btrim(requested_model) and lower(requested_model) <> 'all' and requested_model !~ '[*?,[:space:][:cntrl:]]'),
    route_model text not null check (length(route_model) between 1 and 256 and route_model = btrim(route_model) and lower(route_model) <> 'all' and route_model !~ '[*?,[:space:][:cntrl:]]'),
    target_provider text not null check (length(target_provider) > 0),
    position integer not null check (position >= 0),
    primary key (policy_id, protocol, requested_model),
    unique (policy_id, position)
);
