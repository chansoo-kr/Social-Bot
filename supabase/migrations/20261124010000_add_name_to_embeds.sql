alter table public.embeds
add column name text not null;

alter table public.embeds
add primary key (guild_id, name);

create trigger embeds_set_updated_at before
update on public.embeds for each row execute function public.set_updated_at ();
