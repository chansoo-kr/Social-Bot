create table
    public.moderators (
        guild_id bigint not null references public.guilds (guild_id) on delete cascade,
        user_id bigint not null,
        added_by bigint,
        created_at timestamptz not null default now (),
        primary key (guild_id, user_id)
    );

alter table public.moderators enable row level security;