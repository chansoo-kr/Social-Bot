-- Last locale Discord reported for a user. DMs carry no locale, so this decides their language.
create table
    public.user_locales (user_id bigint primary key, locale text not null);

alter table public.user_locales enable row level security;