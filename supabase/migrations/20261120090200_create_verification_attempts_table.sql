create table
    public.verification_attempts (
        guild_id bigint not null references public.guilds (guild_id) on delete cascade,
        user_id bigint not null,
        last_submitted_at timestamptz not null default now (),
        primary key (guild_id, user_id)
    );

alter table public.verification_attempts enable row level security;