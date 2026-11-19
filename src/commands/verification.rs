use crate::{Context, Data, Error};
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
    fn into_embed(self) -> serenity::CreateEmbed<'static> {
        match self {
            Self::Resolved(embed) => *embed,
            Self::AlreadyHandled => notice(
                COLOR_INFO,
                "Already handled",
                "This request is no longer pending.",
            ),
            Self::NotConfigured => notice(
                COLOR_DANGER,
                "Verification is not configured",
                "Run `/setup verification` first so I know which role to assign.",
            ),
            Self::RoleFailed(error) => notice(
                COLOR_DANGER,
                "Couldn't assign the role",
                format!(
                    "The request is still pending.\n\nCheck that I have **Manage Roles**, that \
                     my role is above the verified role, and that the applicant is still in the \
                     server.\n```\n{error}\n```"
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
    default_member_permissions = "MANAGE_GUILD",
    subcommands("pending", "approve", "reject")
)]
pub async fn verification(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// List pending verification requests, or show one in detail
#[poise::command(slash_command, guild_only)]
async fn pending(
    ctx: Context<'_>,
    #[description = "Show the request submitted by this user"] user: Option<serenity::User>,
    #[description = "Show the request with this ID"] id: Option<i64>,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let pool = &ctx.data().pool;
    ctx.defer_ephemeral().await?;

    if user.is_some() || id.is_some() {
        let Some(pending) = find_pending(pool, guild_id, user.as_ref().map(|u| u.id), id).await?
        else {
            return reply(ctx, not_found()).await;
        };
        let proof_url = fresh_proof_url(ctx.http(), pool, &pending).await;
        ctx.send(
            poise::CreateReply::default()
                .embed(request_embed(&pending, proof_url, None))
                .components(vec![review_buttons(pending.id)])
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
                "Pending Verifications",
                "There are no pending requests.",
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
    let mut footer = String::from("/verification pending id:<id> shows the photo");
    if total > rows.len() as i64 {
        footer = format!("Showing the oldest {} of {total} · {footer}", rows.len());
    }

    reply(
        ctx,
        notice(
            COLOR_PENDING,
            &format!("Pending Verifications ({total})"),
            lines.join("\n"),
        )
        .footer(serenity::CreateEmbedFooter::new(footer)),
    )
    .await
}

/// Approve a pending verification request and assign the verified role
#[poise::command(slash_command, guild_only)]
async fn approve(
    ctx: Context<'_>,
    #[description = "Applicant to approve"] user: Option<serenity::User>,
    #[description = "Request ID to approve"] id: Option<i64>,
) -> Result<(), Error> {
    decide(ctx, user, id, Decision::Approve, None).await
}

/// Reject a pending verification request
#[poise::command(slash_command, guild_only)]
async fn reject(
    ctx: Context<'_>,
    #[description = "Applicant to reject"] user: Option<serenity::User>,
    #[description = "Request ID to reject"] id: Option<i64>,
    #[description = "Reason, sent to the applicant"] reason: Option<String>,
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
    ctx.defer_ephemeral().await?;

    if user.is_none() && id.is_none() {
        return reply(
            ctx,
            notice(
                COLOR_DANGER,
                "Missing target",
                "Specify a `user` or a request `id`.",
            ),
        )
        .await;
    }

    let pool = &ctx.data().pool;
    let Some(pending) = find_pending(pool, guild_id, user.as_ref().map(|u| u.id), id).await? else {
        return reply(ctx, not_found()).await;
    };

    let outcome = resolve(
        ctx.serenity_context(),
        pending.id,
        ctx.author().id,
        decision,
        reason.as_deref(),
    )
    .await?;
    reply(ctx, outcome.into_embed()).await
}

pub async fn reply(ctx: Context<'_>, embed: serenity::CreateEmbed<'_>) -> Result<(), Error> {
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Buttons and the reject-reason modal (moderator DMs and the detail view)
// ---------------------------------------------------------------------------

pub async fn handle_interaction(
    ctx: &serenity::Context,
    interaction: &serenity::Interaction,
) -> Result<(), Error> {
    match interaction {
        serenity::Interaction::Component(component) => {
            let custom_id = component.data.custom_id.as_str();
            if let Some(pending_id) = parse_id(custom_id, APPROVE_PREFIX) {
                component.defer(&ctx.http).await?;
                review_from_interaction(
                    ctx,
                    component.token.as_str(),
                    &component.user,
                    component.guild_id,
                    component.member.as_deref(),
                    pending_id,
                    Decision::Approve,
                    None,
                )
                .await?;
            } else if let Some(pending_id) = parse_id(custom_id, REJECT_PREFIX) {
                show_reject_modal(ctx, component, pending_id).await?;
            }
        }
        serenity::Interaction::Modal(modal) => {
            if let Some(pending_id) = parse_id(modal.data.custom_id.as_str(), REJECT_MODAL_PREFIX) {
                modal.defer(&ctx.http).await?;
                review_from_interaction(
                    ctx,
                    modal.token.as_str(),
                    &modal.user,
                    modal.guild_id,
                    modal.member.as_deref(),
                    pending_id,
                    Decision::Reject,
                    modal_text(modal, "reason"),
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
) -> Result<(), Error> {
    let modal = serenity::CreateModal::new(
        format!("{REJECT_MODAL_PREFIX}{pending_id}"),
        "Reject Verification",
    )
    .components(vec![serenity::CreateModalComponent::Label(
        serenity::CreateLabel::input_text(
            "Reason",
            serenity::CreateInputText::new(serenity::InputTextStyle::Paragraph, "reason")
                .required(false)
                .max_length(500),
        )
        .description("Optional. Sent to the applicant."),
    )]);

    component
        .create_response(&ctx.http, serenity::CreateInteractionResponse::Modal(modal))
        .await?;
    Ok(())
}

/// Runs a decision for an already-acknowledged button or modal interaction.
#[allow(clippy::too_many_arguments)]
async fn review_from_interaction(
    ctx: &serenity::Context,
    token: &str,
    user: &serenity::User,
    guild_id: Option<serenity::GuildId>,
    member: Option<&serenity::Member>,
    pending_id: i64,
    decision: Decision,
    reason: Option<&str>,
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
            // In a server, Manage Server is enough; in DMs only listed moderators may decide.
            let is_manager = guild_id.is_some_and(|g| g.get() as i64 == pending_guild)
                && member
                    .and_then(|m| m.permissions)
                    .is_some_and(|p| p.manage_guild());
            if !is_manager && !is_moderator(pool, pending_guild, user.id).await? {
                serenity::CreateInteractionResponseFollowup::new()
                    .embed(notice(
                        COLOR_DANGER,
                        "Not allowed",
                        "You are not a verification moderator for that server.",
                    ))
                    .ephemeral(true)
                    .execute(&ctx.http, None, token)
                    .await?;
                return Ok(());
            }
            resolve(ctx, pending_id, user.id, decision, reason).await?
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
                .embed(other.into_embed())
                .ephemeral(true)
                .execute(&ctx.http, None, token)
                .await?;
        }
    }
    Ok(())
}

async fn is_moderator(
    pool: &sqlx::PgPool,
    guild_id: i64,
    user_id: serenity::UserId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM moderators WHERE guild_id = $1 AND user_id = $2)",
    )
    .bind(guild_id)
    .bind(user_id.get() as i64)
    .fetch_one(pool)
    .await
}

// ---------------------------------------------------------------------------
// Core review logic
// ---------------------------------------------------------------------------

/// Approves or rejects a pending request, then cleans up moderator DMs and notifies the applicant.
///
/// The row is locked for the whole decision so two moderators can't both act on it, and it is
/// only deleted once the role has actually been assigned.
async fn resolve(
    ctx: &serenity::Context,
    pending_id: i64,
    moderator: serenity::UserId,
    decision: Decision,
    reason: Option<&str>,
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
    let review_messages = sqlx::query_as::<_, (i64, i64)>(
        "SELECT channel_id, message_id FROM verification_review_messages WHERE pending_id = $1",
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

    let embed = result_embed(&pending, decision, moderator, reason);
    retire_review_messages(&ctx.http, &review_messages, &embed).await;
    notify_applicant(ctx, &pending, decision, reason).await;

    Ok(Outcome::Resolved(Box::new(embed)))
}

/// Replaces moderator DMs with `embed`, removing their buttons and the attached proof photo.
async fn retire_review_messages(
    http: &serenity::Http,
    messages: &[(i64, i64)],
    embed: &serenity::CreateEmbed<'static>,
) {
    for &(channel_id, message_id) in messages {
        let edit = serenity::EditMessage::new()
            .embed(embed.clone())
            .components(Vec::<serenity::CreateComponent<'_>>::new())
            .remove_all_attachments();
        if let Err(error) = serenity::GenericChannelId::new(channel_id as u64)
            .edit_message(http, serenity::MessageId::new(message_id as u64), edit)
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
    let guild = guild_name(&ctx.data_ref::<Data>().pool, pending.guild_id).await;
    let embed = match decision {
        Decision::Approve => notice(
            COLOR_SUCCESS,
            "✅ Verification approved",
            format!("Your student verification for **{guild}** was approved."),
        ),
        Decision::Reject => {
            let embed = notice(
                COLOR_DANGER,
                "❌ Verification rejected",
                format!(
                    "Your student verification for **{guild}** was rejected. You can submit a \
                     new request from the verification channel."
                ),
            );
            match reason {
                Some(reason) => embed.field("Reason", clip(reason, 1000), false),
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

/// DMs every moderator of the guild a copy of the request with approve/reject buttons.
///
/// The proof photo is re-uploaded into each DM: the modal upload's own URL is temporary, and a
/// copy the bot owns can be deleted again once the request is resolved.
pub async fn notify_moderators(
    ctx: &serenity::Context,
    pending: &Pending,
    proof: &serenity::Attachment,
) -> Result<(), Error> {
    let pool = &ctx.data_ref::<Data>().pool;

    // A resubmission keeps the same pending row, so DMs from the previous submission would
    // still show the old details and photo.
    let stale = sqlx::query_as::<_, (i64, i64)>(
        "DELETE FROM verification_review_messages WHERE pending_id = $1
        RETURNING channel_id, message_id",
    )
    .bind(pending.id)
    .fetch_all(pool)
    .await?;
    if !stale.is_empty() {
        let superseded = notice(
            COLOR_INFO,
            "Request updated",
            format!(
                "<@{}> submitted request #{} again. See the newer message.",
                pending.user_id, pending.id
            ),
        );
        retire_review_messages(&ctx.http, &stale, &superseded).await;
    }

    let moderators: Vec<i64> =
        sqlx::query_scalar("SELECT user_id FROM moderators WHERE guild_id = $1")
            .bind(pending.guild_id)
            .fetch_all(pool)
            .await?;
    if moderators.is_empty() {
        return Ok(());
    }

    let filename = proof_filename(proof);
    let attachment = serenity::CreateAttachment::bytes(proof.download().await?, filename.clone());
    let guild = guild_name(pool, pending.guild_id).await;
    let embed = request_embed(pending, format!("attachment://{filename}"), Some(&guild));

    for moderator in moderators {
        let message = serenity::CreateMessage::new()
            .embed(embed.clone())
            .add_file(attachment.clone())
            .components(vec![review_buttons(pending.id)]);
        let sent = match serenity::UserId::new(moderator as u64)
            .dm(&ctx.http, message)
            .await
        {
            Ok(sent) => sent,
            Err(error) => {
                warn!(
                    "Failed to DM moderator {moderator} about request {}: {error}",
                    pending.id
                );
                continue;
            }
        };

        let tracked = sqlx::query(
            "INSERT INTO verification_review_messages (message_id, pending_id, channel_id)
            VALUES ($1, $2, $3)",
        )
        .bind(sent.id.get() as i64)
        .bind(pending.id)
        .bind(sent.channel_id.get() as i64)
        .execute(pool)
        .await;
        if let Err(error) = tracked {
            // Most likely the request was resolved while we were sending. An untracked DM
            // would keep its photo forever, so strip it right away.
            warn!("Failed to track review message {}: {error}", sent.id);
            retire_review_messages(
                &ctx.http,
                &[(sent.channel_id.get() as i64, sent.id.get() as i64)],
                &Outcome::AlreadyHandled.into_embed(),
            )
            .await;
        }
    }
    Ok(())
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

pub async fn guild_name(pool: &sqlx::PgPool, guild_id: i64) -> String {
    sqlx::query_scalar::<_, String>("SELECT name FROM guilds WHERE guild_id = $1")
        .bind(guild_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "the server".to_owned())
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

fn not_found() -> serenity::CreateEmbed<'static> {
    notice(
        COLOR_DANGER,
        "Request not found",
        "There is no pending request matching that user or ID in this server.",
    )
}

fn request_embed(
    pending: &Pending,
    image_url: String,
    guild_name: Option<&str>,
) -> serenity::CreateEmbed<'static> {
    let embed = serenity::CreateEmbed::new()
        .title(format!("Verification Request #{}", pending.id))
        .color(COLOR_PENDING)
        .field(
            "Applicant",
            format!("<@{0}> (`{0}`)", pending.user_id),
            false,
        )
        .field("School", clip(&pending.school, 256), true)
        .field("Student Number", clip(&pending.student_number, 256), true)
        .field(
            "Submitted",
            format!("<t:{0}:F> (<t:{0}:R>)", pending.submitted_at),
            false,
        )
        .image(image_url, None);
    match guild_name {
        Some(name) => embed.footer(serenity::CreateEmbedFooter::new(format!("Server: {name}"))),
        None => embed,
    }
}

fn result_embed(
    pending: &Pending,
    decision: Decision,
    moderator: serenity::UserId,
    reason: Option<&str>,
) -> serenity::CreateEmbed<'static> {
    let (title, color) = match decision {
        Decision::Approve => ("✅ Verification Approved", COLOR_SUCCESS),
        Decision::Reject => ("❌ Verification Rejected", COLOR_DANGER),
    };
    let embed = serenity::CreateEmbed::new()
        .title(format!("{title} · #{}", pending.id))
        .color(color)
        .field(
            "Applicant",
            format!("<@{0}> (`{0}`)", pending.user_id),
            false,
        )
        .field("School", clip(&pending.school, 256), true)
        .field("Student Number", clip(&pending.student_number, 256), true)
        .field("Reviewed by", format!("<@{moderator}>"), true)
        .timestamp(serenity::Timestamp::now());
    match reason {
        Some(reason) => embed.field("Reason", clip(reason, 1000), false),
        None => embed,
    }
}

fn review_buttons(pending_id: i64) -> serenity::CreateComponent<'static> {
    serenity::CreateComponent::ActionRow(serenity::CreateActionRow::buttons(vec![
        serenity::CreateButton::new(format!("{APPROVE_PREFIX}{pending_id}"))
            .label("Approve")
            .style(serenity::ButtonStyle::Success),
        serenity::CreateButton::new(format!("{REJECT_PREFIX}{pending_id}"))
            .label("Reject")
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
