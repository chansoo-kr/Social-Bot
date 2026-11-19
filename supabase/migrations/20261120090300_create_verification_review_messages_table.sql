-- DMs sent to moderators for a pending request. Tracked so the bot can strip the
-- proof photo and buttons from every copy once the request is resolved.
create table
    public.verification_review_messages (
        message_id bigint primary key,
        pending_id bigint not null references public.pending_verifications (id) on delete cascade,
        channel_id bigint not null,
        created_at timestamptz not null default now ()
    );

create index verification_review_messages_pending_id_idx on public.verification_review_messages (pending_id);

alter table public.verification_review_messages enable row level security;