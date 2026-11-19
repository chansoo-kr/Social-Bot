create table
    public.user_locales (user_id bigint primary key, locale text not null);

alter table public.user_locales enable row level security;