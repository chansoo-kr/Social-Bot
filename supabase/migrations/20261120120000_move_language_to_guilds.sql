-- The language is a server-wide setting (`/setup language`), not part of the verification setup.
-- Null until it is chosen or detected for the first time.
alter table public.guilds
add column language text check (language in ('en', 'ko'));

update public.guilds g
set language = s.language
from public.verification_settings s
where s.guild_id = g.guild_id and s.language is not null;

alter table public.verification_settings
drop column language;
