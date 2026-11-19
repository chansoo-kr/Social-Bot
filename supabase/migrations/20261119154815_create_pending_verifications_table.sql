create table
    public.pending_verifications (
        id bigint generated always as identity primary key,
        guild_id bigint not null references public.guilds (guild_id) on delete cascade,
        user_id bigint not null,
        school text not null,
        student_number text not null,
        proof_url text not null,
        created_at timestamptz not null default now (),
        updated_at timestamptz not null default now (),
        unique (guild_id, user_id)
    );

alter table public.pending_verifications enable row level security;

create trigger pending_verifications_set_updated_at before
update on public.pending_verifications for each row execute function public.set_updated_at ();