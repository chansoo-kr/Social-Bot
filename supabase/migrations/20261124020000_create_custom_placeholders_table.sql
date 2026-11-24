create table public.custom_placeholders (
    guild_id bigint not null references public.guilds (guild_id) on delete cascade,
    name text not null,
    value text not null,
    created_by bigint,
    created_at timestamptz not null default now (),
    updated_by bigint,
    updated_at timestamptz not null default now (),
    primary key (guild_id, name)
);
alter table public.custom_placeholders enable row level security;

create trigger custom_placeholders_set_updated_at before
update on public.custom_placeholders for each row execute function public.set_updated_at ();
