create table
    public.guilds (
        guild_id bigint primary key,
        name text not null,
        is_active boolean not null default true,
        joined_at timestamptz not null default now (),
        left_at timestamptz,
        updated_at timestamptz not null default now ()
    );

alter table public.guilds enable row level security;