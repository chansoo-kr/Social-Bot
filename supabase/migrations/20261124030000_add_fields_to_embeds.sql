-- Per-embed settings for the modal shown when sending a template: how each [[field]] is asked.
alter table public.embeds
add column fields jsonb not null default '{}'::jsonb;
