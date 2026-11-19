create trigger guilds_set_updated_at before
update on public.guilds for each row execute function public.set_updated_at ();

create trigger verification_settings_set_updated_at before
update on public.verification_settings for each row execute function public.set_updated_at ();