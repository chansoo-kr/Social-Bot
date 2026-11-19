alter table public.verification_settings
add column cooldown_seconds integer not null default 600 check (cooldown_seconds >= 0);