-- Language the DM was written in, so it is edited in the same language once the request is resolved.
alter table public.verification_review_messages
add column locale text not null default 'en';