use crate::{
    Context, Data, Error,
    commands::setup::stored_guild_lang,
    i18n::{self, Lang, t},
};
use poise::serenity_prelude as serenity;
use tracing::warn;

const APPROVE_PREFIX: &str = "verify:approve:";
const REJECT_PREFIX: &str = "verify:reject:";
const REJECT_MODAL_PREFIX: &str = "verify:reject_modal:";

pub const COLOR_INFO: u32 = 0x5865F2;
pub const COLOR_PENDING: u32 = 0xFEE75C;
pub const COLOR_SUCCESS: u32 = 0x57F287;
pub const COLOR_DANGER: u32 = 0xED4245;

const LIST_LIMIT: i64 = 20;

macro_rules! pending_columns {
    () => {
        "id, guild_id, user_id, school, student_number, proof_url, \
         extract(epoch FROM updated_at)::bigint AS submitted_at"
    };
}

#[derive(sqlx::FromRow)]
pub struct Pending {
    pub id: i64,
    pub guild_id: i64,
    pub user_id: i64,
    pub school: String,
    pub student_number: String,
    pub proof_url: String,
    /// Unix seconds of the latest (re)submission.
    pub submitted_at: i64,
}

/// A request DM sent to a moderator: `(channel_id, message_id, locale it was written in)`.
type ReviewMessage = (i64, i64, String);

#[derive(Clone, Copy, PartialEq)]
enum Decision {
    Approve,
    Reject,
}

enum Outcome {
    Resolved(Box<serenity::CreateEmbed<'static>>),
    AlreadyHandled,
    NotConfigured,
    RoleFailed(serenity::Error),
}

impl Outcome {
    fn into_embed(self, lang: Lang) -> serenity::CreateEmbed<'static> {
        match self {
            Self::Resolved(embed) => *embed,
            Self::AlreadyHandled => notice(
                COLOR_INFO,
                t!(lang, "Already handled", "이미 처리된 신청"),
                t!(
                    lang,
                    "This request is no longer pending.",
                    "이 신청은 더 이상 대기 중이 아닙니다."
                ),
            ),
            Self::NotConfigured => notice(
                COLOR_DANGER,
                t!(
                    lang,
                    "Verification is not configured",
                    "인증이 설정되지 않았습니다"
                ),
                t!(
                    lang,
                    "Run `/setup verification` first so I know which role to assign.",
                    "부여할 역할을 알 수 있도록 먼저 `/setup verification`을 실행해 주세요."
                ),
            ),
            Self::RoleFailed(error) => notice(
                COLOR_DANGER,
                t!(
                    lang,
                    "Couldn't assign the role",
                    "역할을 부여하지 못했습니다"
                ),
                t!(
                    lang,
                    format!(
                        "The request is still pending.\n\nCheck that I have **Manage Roles**, \
                         that my role is above the verified role, and that the applicant is \
                         still in the server.\n```\n{error}\n```"
                    ),
                    format!(
                        "신청은 아직 대기 상태입니다.\n\n봇에 **역할 관리** 권한이 있는지, 봇의 \
                         역할이 인증 역할보다 위에 있는지, 신청자가 아직 서버에 있는지 확인해 \
                         주세요.\n```\n{error}\n```"
                    ),
                ),
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

#[poise::command(
    slash_command,
    guild_only,
    check = "moderator_only",
    subcommands("pending", "approve", "reject")
)]
pub async fn verification(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// List pending verification requests, or show one in detail
#[poise::command(
    slash_command,
    guild_only,
    description_localized("ko", "대기 중인 인증 신청 목록을 보거나 한 건을 자세히 봅니다")
)]
async fn pending(
    ctx: Context<'_>,
    #[description = "Show the request submitted by this user"]
    #[description_localized("ko", "이 유저가 제출한 신청 보기")]
    user: Option<serenity::User>,
    #[description = "Show the request with this ID"]
    #[description_localized("ko", "이 ID의 신청 보기")]
    id: Option<i64>,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let lang = i18n::of(ctx);
    let pool = &ctx.data().pool;
    ctx.defer_ephemeral().await?;

    if user.is_some() || id.is_some() {
        let Some(pending) = find_pending(pool, guild_id, user.as_ref().map(|u| u.id), id).await?
        else {
            return reply(ctx, not_found(lang)).await;
        };
        let proof_url = fresh_proof_url(ctx.http(), pool, &pending).await;
        ctx.send(
            poise::CreateReply::default()
                .embed(request_embed(&pending, proof_url, None, lang))
                .components(vec![review_buttons(pending.id, lang)])
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pending_verifications WHERE guild_id = $1")
            .bind(guild_id.get() as i64)
            .fetch_one(pool)
            .await?;
    let rows: Vec<Pending> = sqlx::query_as(concat!(
        "SELECT ",
        pending_columns!(),
        " FROM pending_verifications WHERE guild_id = $1 ORDER BY updated_at LIMIT $2"
    ))
    .bind(guild_id.get() as i64)
    .bind(LIST_LIMIT)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return reply(
            ctx,
            notice(
                COLOR_INFO,
                t!(lang, "Pending Verifications", "대기 중인 인증 신청"),
                t!(
                    lang,
                    "There are no pending requests.",
                    "대기 중인 신청이 없습니다."
                ),
            ),
        )
        .await;
    }

    let lines: Vec<String> = rows
        .iter()
        .map(|p| {
            format!(
                "`#{}` <@{}> · {} · {} · <t:{}:R>",
                p.id,
                p.user_id,
                clip(&p.school, 40),
                clip(&p.student_number, 24),
                p.submitted_at
            )
        })
        .collect();
    let mut footer = t!(
        lang,
        "/verification pending id:<id> shows the photo",
        "/verification pending id:<id> 로 사진을 볼 수 있습니다"
    )
    .to_owned();
    if total > rows.len() as i64 {
        let shown = rows.len();
        footer = t!(
            lang,
            format!("Showing the oldest {shown} of {total} · {footer}"),
            format!("전체 {total}건 중 오래된 {shown}건 표시 · {footer}"),
        );
    }

    reply(
        ctx,
        notice(
            COLOR_PENDING,
            &t!(
                lang,
                format!("Pending Verifications ({total})"),
                format!("대기 중인 인증 신청 ({total})"),
            ),
            lines.join("\n"),
        )
        .footer(serenity::CreateEmbedFooter::new(footer)),
    )
    .await
}

/// Approve a pending verification request and assign the verified role
#[poise::command(
    slash_command,
    guild_only,
    description_localized("ko", "대기 중인 인증 신청을 승인하고 인증 역할을 부여합니다")
)]
async fn approve(
    ctx: Context<'_>,
    #[description = "Applicant to approve"]
    #[description_localized("ko", "승인할 신청자")]
    user: Option<serenity::User>,
    #[description = "Request ID to approve"]
    #[description_localized("ko", "승인할 신청 ID")]
    id: Option<i64>,
) -> Result<(), Error> {
    decide(ctx, user, id, Decision::Approve, None).await
}

/// Reject a pending verification request
#[poise::command(
    slash_command,
    guild_only,
    description_localized("ko", "대기 중인 인증 신청을 거절합니다")
)]
async fn reject(
    ctx: Context<'_>,
    #[description = "Applicant to reject"]
    #[description_localized("ko", "거절할 신청자")]
    user: Option<serenity::User>,
    #[description = "Request ID to reject"]
    #[description_localized("ko", "거절할 신청 ID")]
    id: Option<i64>,
    #[description = "Reason, sent to the applicant"]
    #[description_localized("ko", "신청자에게 전달되는 사유")]
    reason: Option<String>,
) -> Result<(), Error> {
    decide(ctx, user, id, Decision::Reject, reason).await
}

async fn decide(
    ctx: Context<'_>,
    user: Option<serenity::User>,
    id: Option<i64>,
    decision: Decision,
    reason: Option<String>,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    if user.is_none() && id.is_none() {
        return reply(
            ctx,
            notice(
                COLOR_DANGER,
                t!(lang, "Missing target", "대상이 지정되지 않았습니다"),
                t!(
                    lang,
                    "Specify a `user` or a request `id`.",
                    "`user` 또는 신청 `id`를 지정해 주세요."
                ),
            ),
        )
        .await;
    }

    let pool = &ctx.data().pool;
    let Some(pending) = find_pending(pool, guild_id, user.as_ref().map(|u| u.id), id).await? else {
        return reply(ctx, not_found(lang)).await;
    };

    let outcome = resolve(
        ctx.serenity_context(),
        pending.id,
        ctx.author().id,
        decision,
        reason.as_deref(),
        lang,
    )
    .await?;
    reply(ctx, outcome.into_embed(lang)).await
}

pub async fn reply(ctx: Context<'_>, embed: serenity::CreateEmbed<'_>) -> Result<(), Error> {
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Buttons and the reject-reason modal (request messages and the detail view)
// ---------------------------------------------------------------------------

pub async fn handle_interaction(
    ctx: &serenity::Context,
    interaction: &serenity::Interaction,
) -> Result<(), Error> {
    match interaction {
        serenity::Interaction::Component(component) => {
            let custom_id = component.data.custom_id.as_str();
            let lang = Lang::from_locale(&component.locale);
            if let Some(pending_id) = parse_id(custom_id, APPROVE_PREFIX) {
                component.defer(&ctx.http).await?;
                review_from_interaction(
                    ctx,
                    component.token.as_str(),
                    &component.user,
                    pending_id,
                    Decision::Approve,
                    None,
                    lang,
                )
                .await?;
            } else if let Some(pending_id) = parse_id(custom_id, REJECT_PREFIX) {
                show_reject_modal(ctx, component, pending_id, lang).await?;
            }
        }
        serenity::Interaction::Modal(modal) => {
            if let Some(pending_id) = parse_id(modal.data.custom_id.as_str(), REJECT_MODAL_PREFIX) {
                modal.defer(&ctx.http).await?;
                review_from_interaction(
                    ctx,
                    modal.token.as_str(),
                    &modal.user,
                    pending_id,
                    Decision::Reject,
                    modal_text(modal, "reason"),
                    Lang::from_locale(&modal.locale),
                )
                .await?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_id(custom_id: &str, prefix: &str) -> Option<i64> {
    custom_id.strip_prefix(prefix)?.parse().ok()
}

fn modal_text<'a>(modal: &'a serenity::ModalInteraction, custom_id: &str) -> Option<&'a str> {
    modal.data.components.iter().find_map(|component| {
        let serenity::ModalComponent::Label(label) = component else {
            return None;
        };
        match &label.component {
            serenity::LabelComponent::InputText(input) if input.custom_id.as_str() == custom_id => {
                Some(input.value.trim()).filter(|value| !value.is_empty())
            }
            _ => None,
        }
    })
}

async fn show_reject_modal(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    pending_id: i64,
    lang: Lang,
) -> Result<(), Error> {
    let modal = serenity::CreateModal::new(
        format!("{REJECT_MODAL_PREFIX}{pending_id}"),
        t!(lang, "Reject Verification", "인증 거절"),
    )
    .components(vec![serenity::CreateModalComponent::Label(
        serenity::CreateLabel::input_text(
            t!(lang, "Reason", "사유"),
            serenity::CreateInputText::new(serenity::InputTextStyle::Paragraph, "reason")
                .required(false)
                .max_length(500),
        )
        .description(t!(
            lang,
            "Optional. Sent to the applicant.",
            "선택 사항입니다. 신청자에게 전달됩니다."
        )),
    )]);

    component
        .create_response(&ctx.http, serenity::CreateInteractionResponse::Modal(modal))
        .await?;
    Ok(())
}

/// Runs a decision for an already-acknowledged button or modal interaction.
async fn review_from_interaction(
    ctx: &serenity::Context,
    token: &str,
    user: &serenity::User,
    pending_id: i64,
    decision: Decision,
    reason: Option<&str>,
    lang: Lang,
) -> Result<(), Error> {
    let pool = &ctx.data_ref::<Data>().pool;

    let pending_guild: Option<i64> =
        sqlx::query_scalar("SELECT guild_id FROM pending_verifications WHERE id = $1")
            .bind(pending_id)
            .fetch_optional(pool)
            .await?;

    let outcome = match pending_guild {
        None => Outcome::AlreadyHandled,
        Some(pending_guild) => {
            // Same rule as the slash commands, whether the button was pressed in a DM or in the
            // log channel.
            let guild_id = serenity::GuildId::new(pending_guild as u64);
            if !can_moderate(ctx, guild_id, user.id).await? {
                serenity::CreateInteractionResponseFollowup::new()
                    .embed(notice(
                        COLOR_DANGER,
                        t!(lang, "Not allowed", "권한이 없습니다"),
                        t!(
                            lang,
                            "You are not a verification moderator for that server.",
                            "해당 서버의 인증 모더레이터가 아닙니다."
                        ),
                    ))
                    .ephemeral(true)
                    .execute(&ctx.http, None, token)
                    .await?;
                return Ok(());
            }
            resolve(ctx, pending_id, user.id, decision, reason, lang).await?
        }
    };

    match outcome {
        // Replaces the message the button was on, dropping its photo and buttons.
        Outcome::Resolved(embed) => {
            serenity::EditInteractionResponse::new()
                .embed(*embed)
                .components(Vec::<serenity::CreateComponent<'_>>::new())
                .clear_attachments()
                .execute(&ctx.http, token)
                .await?;
        }
        other => {
            serenity::CreateInteractionResponseFollowup::new()
                .embed(other.into_embed(lang))
                .ephemeral(true)
                .execute(&ctx.http, None, token)
                .await?;
        }
    }
    Ok(())
}

/// Whether a user may run the bot's management commands and decide requests for a guild: anyone
/// on its moderator list, plus the server owner so the list can be started and never locks
/// everyone out.
pub async fn can_moderate(
    ctx: &serenity::Context,
    guild_id: serenity::GuildId,
    user_id: serenity::UserId,
) -> Result<bool, Error> {
    let listed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM moderators WHERE guild_id = $1 AND user_id = $2)",
    )
    .bind(guild_id.get() as i64)
    .bind(user_id.get() as i64)
    .fetch_one(&ctx.data_ref::<Data>().pool)
    .await?;
    if listed {
        return Ok(true);
    }

    let cached_owner = ctx.cache.guild(guild_id).map(|guild| guild.owner_id);
    let owner = match cached_owner {
        Some(owner) => owner,
        None => ctx.http.get_guild(guild_id).await?.owner_id,
    };
    Ok(owner == user_id)
}

/// Command check for `/setup` and `/verification`; subcommands inherit it from their parent.
pub async fn moderator_only(ctx: Context<'_>) -> Result<bool, Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(false);
    };
    if can_moderate(ctx.serenity_context(), guild_id, ctx.author().id).await? {
        return Ok(true);
    }

    let lang = i18n::of(ctx);
    reply(
        ctx,
        notice(
            COLOR_DANGER,
            t!(lang, "Not allowed", "권한이 없습니다"),
            t!(
                lang,
                "Only this server's moderators can use this command. The server owner can add \
                 moderators with `/setup moderator add`.",
                "이 서버의 모더레이터만 사용할 수 있는 명령입니다. 서버 소유자가 `/setup \
                 moderator add`로 모더레이터를 추가할 수 있습니다."
            ),
        ),
    )
    .await?;
    Ok(false)
}

// ---------------------------------------------------------------------------
// Core review logic
// ---------------------------------------------------------------------------

/// Approves or rejects a pending request, then cleans up the request messages and notifies the
/// applicant.
///
/// The row is locked for the whole decision so two moderators can't both act on it, and it is
/// only deleted once the role has actually been assigned. `lang` is the acting moderator's
/// language and only affects the returned embed.
async fn resolve(
    ctx: &serenity::Context,
    pending_id: i64,
    moderator: serenity::UserId,
    decision: Decision,
    reason: Option<&str>,
    lang: Lang,
) -> Result<Outcome, Error> {
    let pool = &ctx.data_ref::<Data>().pool;
    let mut tx = pool.begin().await?;

    let Some(pending) = sqlx::query_as::<_, Pending>(concat!(
        "SELECT ",
        pending_columns!(),
        " FROM pending_verifications WHERE id = $1 FOR UPDATE"
    ))
    .bind(pending_id)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(Outcome::AlreadyHandled);
    };

    // Read before the delete below cascades these rows away.
    let review_messages = sqlx::query_as::<_, ReviewMessage>(
        "SELECT channel_id, message_id, locale FROM verification_review_messages
        WHERE pending_id = $1",
    )
    .bind(pending_id)
    .fetch_all(&mut *tx)
    .await?;

    if decision == Decision::Approve {
        let role_id: Option<i64> =
            sqlx::query_scalar("SELECT role_id FROM verification_settings WHERE guild_id = $1")
                .bind(pending.guild_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(role_id) = role_id else {
            return Ok(Outcome::NotConfigured);
        };
        if let Err(error) = ctx
            .http
            .add_member_role(
                serenity::GuildId::new(pending.guild_id as u64),
                serenity::UserId::new(pending.user_id as u64),
                serenity::RoleId::new(role_id as u64),
                Some("Student verification approved"),
            )
            .await
        {
            return Ok(Outcome::RoleFailed(error));
        }
    }

    sqlx::query("DELETE FROM pending_verifications WHERE id = $1")
        .bind(pending_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    retire_review_messages(&ctx.http, &review_messages, |lang| {
        result_embed(&pending, decision, moderator, reason, lang)
    })
    .await;
    notify_applicant(ctx, &pending, decision, reason).await;

    Ok(Outcome::Resolved(Box::new(result_embed(
        &pending, decision, moderator, reason, lang,
    ))))
}

/// Replaces request messages (moderator DMs, log channel) with the embed in each one's language,
/// removing their buttons and the attached proof photo.
async fn retire_review_messages(
    http: &serenity::Http,
    messages: &[ReviewMessage],
    embed_for: impl Fn(Lang) -> serenity::CreateEmbed<'static>,
) {
    for (channel_id, message_id, locale) in messages {
        let edit = serenity::EditMessage::new()
            .embed(embed_for(Lang::from_locale(locale)))
            .components(Vec::<serenity::CreateComponent<'_>>::new())
            .remove_all_attachments();
        if let Err(error) = serenity::GenericChannelId::new(*channel_id as u64)
            .edit_message(http, serenity::MessageId::new(*message_id as u64), edit)
            .await
        {
            warn!("Failed to clean up review message {message_id}: {error}");
        }
    }
}

async fn notify_applicant(
    ctx: &serenity::Context,
    pending: &Pending,
    decision: Decision,
    reason: Option<&str>,
) {
    let pool = &ctx.data_ref::<Data>().pool;
    let lang = i18n::lang_of(pool, pending.user_id).await;
    let guild = guild_name(pool, pending.guild_id, lang).await;
    let embed = match decision {
        Decision::Approve => notice(
            COLOR_SUCCESS,
            t!(lang, "✅ Verification approved", "✅ 인증이 승인되었습니다"),
            t!(
                lang,
                format!("Your student verification for **{guild}** was approved."),
                format!("**{guild}**의 학생 인증이 승인되었습니다."),
            ),
        ),
        Decision::Reject => {
            let embed = notice(
                COLOR_DANGER,
                t!(lang, "❌ Verification rejected", "❌ 인증이 거절되었습니다"),
                t!(
                    lang,
                    format!(
                        "Your student verification for **{guild}** was rejected. You can submit \
                         a new request from the verification channel."
                    ),
                    format!(
                        "**{guild}**의 학생 인증이 거절되었습니다. 인증 채널에서 다시 신청할 수 \
                         있습니다."
                    ),
                ),
            );
            match reason {
                Some(reason) => embed.field(t!(lang, "Reason", "사유"), clip(reason, 1000), false),
                None => embed,
            }
        }
    };

    if let Err(error) = serenity::UserId::new(pending.user_id as u64)
        .dm(&ctx.http, serenity::CreateMessage::new().embed(embed))
        .await
    {
        warn!(
            "Failed to DM applicant {} about their verification result: {error}",
            pending.user_id
        );
    }
}

// ---------------------------------------------------------------------------
// Submission side (called from the applicant modal handler)
// ---------------------------------------------------------------------------

pub async fn upsert_pending(
    conn: &mut sqlx::PgConnection,
    guild_id: serenity::GuildId,
    user_id: serenity::UserId,
    school: &str,
    student_number: &str,
    proof_url: &str,
) -> Result<Pending, sqlx::Error> {
    sqlx::query_as(concat!(
        "INSERT INTO pending_verifications (guild_id, user_id, school, student_number, proof_url)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (guild_id, user_id) DO UPDATE
        SET school = EXCLUDED.school, student_number = EXCLUDED.student_number,
        proof_url = EXCLUDED.proof_url
        RETURNING ",
        pending_columns!()
    ))
    .bind(guild_id.get() as i64)
    .bind(user_id.get() as i64)
    .bind(school)
    .bind(student_number)
    .bind(proof_url)
    .fetch_one(conn)
    .await
}

/// Sends the request, with approve/reject buttons, to the guild's log channel (if one is set) in
/// the server's language and to every moderator's DMs in that moderator's last known language.
///
/// The proof photo is re-uploaded into each message: the modal upload's own URL is temporary,
/// and a copy the bot owns can be deleted again once the request is resolved.
pub async fn announce_request(
    ctx: &serenity::Context,
    pending: &Pending,
    proof: &serenity::Attachment,
) -> Result<(), Error> {
    let pool = &ctx.data_ref::<Data>().pool;

    // A resubmission keeps the same pending row, so messages from the previous submission would
    // still show the old details and photo.
    let stale = sqlx::query_as::<_, ReviewMessage>(
        "DELETE FROM verification_review_messages WHERE pending_id = $1
        RETURNING channel_id, message_id, locale",
    )
    .bind(pending.id)
    .fetch_all(pool)
    .await?;
    retire_review_messages(&ctx.http, &stale, |lang| {
        notice(
            COLOR_INFO,
            t!(lang, "Request updated", "신청이 갱신되었습니다"),
            t!(
                lang,
                format!(
                    "<@{}> submitted request #{} again. See the newer message.",
                    pending.user_id, pending.id
                ),
                format!(
                    "<@{}> 님이 신청(#{})을 다시 제출했습니다. 최신 메시지를 확인해 주세요.",
                    pending.user_id, pending.id
                ),
            ),
        )
    })
    .await;

    let moderators = sqlx::query_as::<_, (i64, Option<String>)>(
        "SELECT m.user_id, l.locale FROM moderators m
        LEFT JOIN user_locales l ON l.user_id = m.user_id
        WHERE m.guild_id = $1",
    )
    .bind(pending.guild_id)
    .fetch_all(pool)
    .await?;
    let log_channel: Option<i64> =
        sqlx::query_scalar("SELECT log_channel_id FROM verification_settings WHERE guild_id = $1")
            .bind(pending.guild_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    if moderators.is_empty() && log_channel.is_none() {
        return Ok(());
    }

    let filename = proof_filename(proof);
    let attachment = serenity::CreateAttachment::bytes(proof.download().await?, filename.clone());

    if let Some(channel_id) = log_channel {
        // A channel is read by everyone in it, so it uses the server's language.
        let guild_id = serenity::GuildId::new(pending.guild_id as u64);
        let lang = stored_guild_lang(pool, guild_id)
            .await
            .ok()
            .flatten()
            .unwrap_or(Lang::En);
        let message = review_message(pending, &attachment, &filename, None, lang);
        match serenity::GenericChannelId::new(channel_id as u64)
            .send_message(&ctx.http, message)
            .await
        {
            Ok(sent) => track_review_message(ctx, pending.id, &sent, lang).await,
            Err(error) => warn!(
                "Failed to post request {} to log channel {channel_id}: {error}",
                pending.id
            ),
        }
    }

    for (moderator, locale) in moderators {
        let lang = locale.as_deref().map_or(Lang::En, Lang::from_locale);
        let guild = guild_name(pool, pending.guild_id, lang).await;
        let message = review_message(pending, &attachment, &filename, Some(&guild), lang);
        match serenity::UserId::new(moderator as u64)
            .dm(&ctx.http, message)
            .await
        {
            Ok(sent) => track_review_message(ctx, pending.id, &sent, lang).await,
            Err(error) => warn!(
                "Failed to DM moderator {moderator} about request {}: {error}",
                pending.id
            ),
        }
    }
    Ok(())
}

fn review_message(
    pending: &Pending,
    attachment: &serenity::CreateAttachment<'static>,
    filename: &str,
    guild_name: Option<&str>,
    lang: Lang,
) -> serenity::CreateMessage<'static> {
    serenity::CreateMessage::new()
        .embed(request_embed(
            pending,
            format!("attachment://{filename}"),
            guild_name,
            lang,
        ))
        .add_file(attachment.clone())
        .components(vec![review_buttons(pending.id, lang)])
}

/// Remembers a sent request message so it can be stripped of its photo and buttons once the
/// request is resolved.
async fn track_review_message(
    ctx: &serenity::Context,
    pending_id: i64,
    sent: &serenity::Message,
    lang: Lang,
) {
    let tracked = sqlx::query(
        "INSERT INTO verification_review_messages (message_id, pending_id, channel_id, locale)
        VALUES ($1, $2, $3, $4)",
    )
    .bind(sent.id.get() as i64)
    .bind(pending_id)
    .bind(sent.channel_id.get() as i64)
    .bind(lang.code())
    .execute(&ctx.data_ref::<Data>().pool)
    .await;
    if let Err(error) = tracked {
        // Most likely the request was resolved while we were sending. An untracked message
        // would keep its photo forever, so strip it right away.
        warn!("Failed to track review message {}: {error}", sent.id);
        let untracked = (
            sent.channel_id.get() as i64,
            sent.id.get() as i64,
            lang.code().to_owned(),
        );
        retire_review_messages(&ctx.http, &[untracked], |lang| {
            Outcome::AlreadyHandled.into_embed(lang)
        })
        .await;
    }
}

/// Discord only resolves `attachment://` references for plain ASCII filenames.
fn proof_filename(proof: &serenity::Attachment) -> String {
    let extension = proof
        .filename
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("png");
    format!("proof.{}", extension.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// Shared lookups and embeds
// ---------------------------------------------------------------------------

async fn find_pending(
    pool: &sqlx::PgPool,
    guild_id: serenity::GuildId,
    user: Option<serenity::UserId>,
    id: Option<i64>,
) -> Result<Option<Pending>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        pending_columns!(),
        " FROM pending_verifications
        WHERE guild_id = $1
        AND ($2::bigint IS NULL OR id = $2)
        AND ($3::bigint IS NULL OR user_id = $3)"
    ))
    .bind(guild_id.get() as i64)
    .bind(id)
    .bind(user.map(|u| u.get() as i64))
    .fetch_optional(pool)
    .await
}

/// Attachment URLs are signed and expire, so ask Discord for a current one from a moderator DM.
/// Falls back to the original modal upload when no moderator DM exists.
async fn fresh_proof_url(http: &serenity::Http, pool: &sqlx::PgPool, pending: &Pending) -> String {
    let messages = sqlx::query_as::<_, (i64, i64)>(
        "SELECT channel_id, message_id FROM verification_review_messages WHERE pending_id = $1",
    )
    .bind(pending.id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for (channel_id, message_id) in messages {
        let message = http
            .get_message(
                serenity::GenericChannelId::new(channel_id as u64),
                serenity::MessageId::new(message_id as u64),
            )
            .await;
        if let Ok(message) = message
            && let Some(attachment) = message.attachments.first()
        {
            return attachment.url.to_string();
        }
    }
    pending.proof_url.clone()
}

pub async fn guild_name(pool: &sqlx::PgPool, guild_id: i64, lang: Lang) -> String {
    sqlx::query_scalar::<_, String>("SELECT name FROM guilds WHERE guild_id = $1")
        .bind(guild_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| t!(lang, "the server", "해당 서버").to_owned())
}

pub fn notice(
    color: u32,
    title: &str,
    description: impl Into<String>,
) -> serenity::CreateEmbed<'static> {
    serenity::CreateEmbed::new()
        .title(title.to_owned())
        .description(description.into())
        .color(color)
}

fn not_found(lang: Lang) -> serenity::CreateEmbed<'static> {
    notice(
        COLOR_DANGER,
        t!(lang, "Request not found", "신청을 찾을 수 없습니다"),
        t!(
            lang,
            "There is no pending request matching that user or ID in this server.",
            "이 서버에 해당 유저 또는 ID와 일치하는 대기 중인 신청이 없습니다."
        ),
    )
}

fn request_embed(
    pending: &Pending,
    image_url: String,
    guild_name: Option<&str>,
    lang: Lang,
) -> serenity::CreateEmbed<'static> {
    let embed = serenity::CreateEmbed::new()
        .title(t!(
            lang,
            format!("Verification Request #{}", pending.id),
            format!("인증 신청 #{}", pending.id),
        ))
        .color(COLOR_PENDING)
        .field(
            t!(lang, "Applicant", "신청자"),
            format!("<@{0}> (`{0}`)", pending.user_id),
            false,
        )
        .field(t!(lang, "School", "학교"), clip(&pending.school, 256), true)
        .field(
            t!(lang, "Student Number", "학번"),
            clip(&pending.student_number, 256),
            true,
        )
        .field(
            t!(lang, "Submitted", "제출 시각"),
            format!("<t:{0}:F> (<t:{0}:R>)", pending.submitted_at),
            false,
        )
        .image(image_url, None);
    match guild_name {
        Some(name) => embed.footer(serenity::CreateEmbedFooter::new(t!(
            lang,
            format!("Server: {name}"),
            format!("서버: {name}"),
        ))),
        None => embed,
    }
}

fn result_embed(
    pending: &Pending,
    decision: Decision,
    moderator: serenity::UserId,
    reason: Option<&str>,
    lang: Lang,
) -> serenity::CreateEmbed<'static> {
    let (title, color) = match decision {
        Decision::Approve => (
            t!(lang, "✅ Verification Approved", "✅ 인증 승인"),
            COLOR_SUCCESS,
        ),
        Decision::Reject => (
            t!(lang, "❌ Verification Rejected", "❌ 인증 거절"),
            COLOR_DANGER,
        ),
    };
    let embed = serenity::CreateEmbed::new()
        .title(format!("{title} · #{}", pending.id))
        .color(color)
        .field(
            t!(lang, "Applicant", "신청자"),
            format!("<@{0}> (`{0}`)", pending.user_id),
            false,
        )
        .field(t!(lang, "School", "학교"), clip(&pending.school, 256), true)
        .field(
            t!(lang, "Student Number", "학번"),
            clip(&pending.student_number, 256),
            true,
        )
        .field(
            t!(lang, "Reviewed by", "처리자"),
            format!("<@{moderator}>"),
            true,
        )
        .timestamp(serenity::Timestamp::now());
    match reason {
        Some(reason) => embed.field(t!(lang, "Reason", "사유"), clip(reason, 1000), false),
        None => embed,
    }
}

fn review_buttons(pending_id: i64, lang: Lang) -> serenity::CreateComponent<'static> {
    serenity::CreateComponent::ActionRow(serenity::CreateActionRow::buttons(vec![
        serenity::CreateButton::new(format!("{APPROVE_PREFIX}{pending_id}"))
            .label(t!(lang, "Approve", "승인"))
            .style(serenity::ButtonStyle::Success),
        serenity::CreateButton::new(format!("{REJECT_PREFIX}{pending_id}"))
            .label(t!(lang, "Reject", "거절"))
            .style(serenity::ButtonStyle::Danger),
    ]))
}

/// Truncates user-provided text so it can't overflow Discord's embed limits.
fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(max_chars - 1).collect();
    clipped.push('…');
    clipped
}
