alter table public.verification_settings
add column language text check (language in ('en', 'ko'));
