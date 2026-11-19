create table
    public.bot_settings (
        singleton boolean primary key default true check (singleton),
        status_text text,
        updated_by bigint,
        updated_at timestamptz not null default now ()
    );

alter table public.bot_settings enable row level security;