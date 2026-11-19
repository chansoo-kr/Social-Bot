-- Language of the public verification panel. Null until the first setup decides it.
alter table public.verification_settings
add column language text check (language in ('en', 'ko'));
