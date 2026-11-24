create table public.embeds (
    guild_id bigint not null references public.guilds (guild_id) on delete cascade,
    data jsonb not null,
    created_by bigint,
    created_at timestamptz not null default now (),
    updated_by bigint,
    updated_at timestamptz not null default now ()
);
alter table public.embeds enable row level security;