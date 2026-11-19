create table
    public.verification_settings (
        guild_id bigint primary key references public.guilds (guild_id) on delete cascade,
        channel_id bigint not null,
        role_id bigint not null,
        message_id bigint,
        enabled boolean not null default true,
        updated_by bigint,
        created_at timestamptz not null default now (),
        updated_at timestamptz not null default now ()
    );

alter table public.verification_settings enable row level security;