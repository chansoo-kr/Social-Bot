use std::borrow::Cow;

use crate::{
    Context, Data, Error,
    commands::verification::{self, COLOR_DANGER, COLOR_INFO, COLOR_SUCCESS, notice, reply},
};
use poise::serenity_prelude::{self as serenity, Mentionable};

pub const BUTTON_ID: &str = "verify:start";
pub const MODAL_ID: &str = "verify:modal";

/// Bots can't re-upload anything larger than this to a DM.
const MAX_PROOF_BYTES: u32 = 10 * 1024 * 1024;

#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    subcommands("verification", "moderator")
)]
pub async fn setup(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

pub fn panel_embed() -> serenity::CreateEmbed<'static> {
    serenity::CreateEmbed::new()
        .title("Student Verification")
        .description("Press the button below and fill in the form to get verified.")
        .color(0x5865F2)
}

pub fn panel_button() -> serenity::CreateButton<'static> {
    serenity::CreateButton::new(BUTTON_ID)
        .label("Verify")
        .style(serenity::ButtonStyle::Primary)
}

pub async fn handle_interaction(
    ctx: &serenity::Context,
    interaction: &serenity::Interaction,
) -> Result<(), Error> {
    if let serenity::Interaction::Component(component) = interaction {
        if component.data.custom_id.as_str() == BUTTON_ID {
            show_modal(ctx, component).await?;
        }
    }

    if let serenity::Interaction::Modal(modal) = interaction {
        if modal.data.custom_id.as_str() == MODAL_ID {
            handle_verification(ctx, modal).await?;
        }
    }
    Ok(())
}

async fn show_modal(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
) -> Result<(), Error> {
    let Some(guild_id) = component.guild_id else {
        return Ok(());
    };

    // Checked again on submit; this just saves the user from filling in a form that will bounce.
    let pool = &ctx.data_ref::<Data>().pool;
    if let Some(until) = cooldown_until(pool, guild_id, component.user.id).await? {
        component
            .create_response(&ctx.http, ephemeral_embed(cooldown_notice(until)))
            .await?;
        return Ok(());
    }

    let modal = serenity::CreateModal::new(MODAL_ID, "Student Verification").components(vec![
        serenity::CreateModalComponent::Label(serenity::CreateLabel::input_text(
            "School",
            serenity::CreateInputText::new(serenity::InputTextStyle::Short, "school")
                .max_length(100),
        )),
        serenity::CreateModalComponent::Label(serenity::CreateLabel::input_text(
            "Student Number",
            serenity::CreateInputText::new(serenity::InputTextStyle::Short, "student_number")
                .max_length(32),
        )),
        serenity::CreateModalComponent::Label(
            serenity::CreateLabel::file_upload(
                "Student ID Photo",
                serenity::CreateFileUpload::new("proof")
                    .max_values(1)
                    .file_types(vec![Cow::Borrowed("image")]),
            )
            .description("Please cover any unnecessary personal information before uploading"),
        ),
    ]);

    component
        .create_response(&ctx.http, serenity::CreateInteractionResponse::Modal(modal))
        .await?;
    Ok(())
}

pub async fn handle_verification(
    ctx: &serenity::Context,
    modal: &serenity::ModalInteraction,
) -> Result<(), Error> {
    let Some(guild_id) = modal.guild_id else {
        return Ok(());
    };

    let mut school = None;
    let mut student_number = None;
    let mut proof_id = None;

    for component in &modal.data.components {
        let serenity::ModalComponent::Label(label) = component else {
            continue;
        };
        match &label.component {
            serenity::LabelComponent::InputText(input) => match input.custom_id.as_str() {
                "school" => school = Some(input.value.trim()),
                "student_number" => student_number = Some(input.value.trim()),
                _ => {}
            },
            serenity::LabelComponent::FileUpload(upload)
                if upload.custom_id.as_str() == "proof" =>
            {
                proof_id = upload.values.first().copied();
            }
            _ => {}
        }
    }

    let proof = proof_id.and_then(|id| modal.data.resolved.attachments.get(&id));

    let (Some(school), Some(student_number), Some(proof)) = (
        school.filter(|s| !s.is_empty()),
        student_number.filter(|s| !s.is_empty()),
        proof,
    ) else {
        respond_ephemeral(ctx, modal, "Please fill in every field and attach a photo.").await?;
        return Ok(());
    };

    let is_image = proof
        .content_type
        .as_deref()
        .is_some_and(|content_type| content_type.starts_with("image/"));
    if !is_image || proof.size > MAX_PROOF_BYTES {
        respond_ephemeral(ctx, modal, "The photo must be an image under 10 MB.").await?;
        return Ok(());
    }

    let pool = &ctx.data_ref::<Data>().pool;

    let cooldown_seconds: Option<i32> = sqlx::query_scalar(
        "SELECT cooldown_seconds FROM verification_settings WHERE guild_id = $1 AND enabled",
    )
    .bind(guild_id.get() as i64)
    .fetch_optional(pool)
    .await?;
    let Some(cooldown_seconds) = cooldown_seconds else {
        respond_ephemeral(
            ctx,
            modal,
            "Verification is not available on this server right now.",
        )
        .await?;
        return Ok(());
    };

    let mut tx = pool.begin().await?;

    // Atomically starts a new cooldown window; touches no row while the previous one is running.
    let cooldown_claimed = sqlx::query(
        "INSERT INTO verification_attempts (guild_id, user_id) VALUES ($1, $2)
        ON CONFLICT (guild_id, user_id) DO UPDATE SET last_submitted_at = now()
        WHERE verification_attempts.last_submitted_at + $3::int * interval '1 second' <= now()",
    )
    .bind(guild_id.get() as i64)
    .bind(modal.user.id.get() as i64)
    .bind(cooldown_seconds)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;

    if !cooldown_claimed {
        tx.rollback().await?;
        let embed = match cooldown_until(pool, guild_id, modal.user.id).await? {
            Some(until) => cooldown_notice(until),
            None => notice(COLOR_INFO, "Please wait", "Try again in a moment."),
        };
        modal
            .create_response(&ctx.http, ephemeral_embed(embed))
            .await?;
        return Ok(());
    }

    let pending = verification::upsert_pending(
        &mut tx,
        guild_id,
        modal.user.id,
        school,
        student_number,
        proof.url.as_str(),
    )
    .await?;
    tx.commit().await?;

    // Respond before notifying moderators: re-uploading the photo can outlast the 3 second window.
    modal
        .create_response(
            &ctx.http,
            ephemeral_embed(notice(
                COLOR_SUCCESS,
                "Request submitted",
                "A moderator will review your request. I'll DM you the result if your DMs are open.",
            )),
        )
        .await?;

    verification::notify_moderators(ctx, &pending, proof).await
}

/// Unix seconds at which the user may submit again, or `None` if they can submit now.
async fn cooldown_until(
    pool: &sqlx::PgPool,
    guild_id: serenity::GuildId,
    user_id: serenity::UserId,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT ceil(extract(epoch FROM a.last_submitted_at
            + s.cooldown_seconds * interval '1 second'))::bigint
        FROM verification_attempts a
        JOIN verification_settings s USING (guild_id)
        WHERE a.guild_id = $1 AND a.user_id = $2
        AND a.last_submitted_at + s.cooldown_seconds * interval '1 second' > now()",
    )
    .bind(guild_id.get() as i64)
    .bind(user_id.get() as i64)
    .fetch_optional(pool)
    .await
}

fn cooldown_notice(until: i64) -> serenity::CreateEmbed<'static> {
    notice(
        COLOR_INFO,
        "Please wait",
        format!("You submitted a request recently. You can submit again <t:{until}:R>."),
    )
}

fn ephemeral_embed(embed: serenity::CreateEmbed<'_>) -> serenity::CreateInteractionResponse<'_> {
    serenity::CreateInteractionResponse::Message(
        serenity::CreateInteractionResponseMessage::new()
            .embed(embed)
            .ephemeral(true),
    )
}

async fn respond_ephemeral(
    ctx: &serenity::Context,
    modal: &serenity::ModalInteraction,
    content: &str,
) -> Result<(), Error> {
    modal
        .create_response(
            &ctx.http,
            serenity::CreateInteractionResponse::Message(
                serenity::CreateInteractionResponseMessage::new()
                    .content(content)
                    .ephemeral(true),
            ),
        )
        .await?;
    Ok(())
}

/// Post the verification panel and choose the role given to verified students
#[poise::command(slash_command, guild_only)]
async fn verification(
    ctx: Context<'_>,
    #[description = "Verification channel"] channel: serenity::GuildChannel,
    #[description = "Role to assign after verification"] role: serenity::Role,
    #[description = "Minutes a user must wait between requests (default 10)"]
    #[min = 0]
    #[max = 10080]
    cooldown_minutes: Option<i32>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;

    let panel = channel
        .send_message(
            ctx.http(),
            serenity::CreateMessage::new()
                .embed(panel_embed())
                .components(vec![serenity::CreateComponent::ActionRow(
                    serenity::CreateActionRow::buttons(vec![panel_button()]),
                )]),
        )
        .await?;

    // 600 mirrors the column default; an omitted option keeps whatever was configured before.
    let cooldown_seconds: i32 = sqlx::query_scalar(
        "INSERT INTO verification_settings
            (guild_id, channel_id, role_id, message_id, updated_by, cooldown_seconds)
        VALUES ($1, $2, $3, $4, $5, COALESCE($6, 600))
        ON CONFLICT (guild_id) DO UPDATE
        SET channel_id = EXCLUDED.channel_id, role_id = EXCLUDED.role_id,
        message_id = EXCLUDED.message_id, updated_by = EXCLUDED.updated_by,
        cooldown_seconds = COALESCE($6, verification_settings.cooldown_seconds)
        RETURNING cooldown_seconds",
    )
    .bind(channel.base.guild_id.get() as i64)
    .bind(channel.id.get() as i64)
    .bind(role.id.get() as i64)
    .bind(panel.id.get() as i64)
    .bind(ctx.author().id.get() as i64)
    .bind(cooldown_minutes.map(|minutes| minutes * 60))
    .fetch_one(&ctx.data().pool)
    .await?;

    let embed = serenity::CreateEmbed::new()
        .title("✅ Verification setup complete")
        .color(0x57F287)
        .field("Channel", channel.mention().to_string(), true)
        .field("Role", role.mention().to_string(), true)
        .field("Cooldown", format!("{} min", cooldown_seconds / 60), true)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "Configured by {}",
            ctx.author().name
        )))
        .timestamp(serenity::Timestamp::now());

    reply(ctx, embed).await
}

#[poise::command(
    slash_command,
    guild_only,
    subcommands("moderator_add", "moderator_remove", "moderator_list")
)]
async fn moderator(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// DM this user whenever a new verification request comes in
#[poise::command(slash_command, guild_only, rename = "add")]
async fn moderator_add(
    ctx: Context<'_>,
    #[description = "User to notify about new requests"] user: serenity::User,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    ctx.defer_ephemeral().await?;

    if user.bot() {
        return reply(
            ctx,
            notice(COLOR_DANGER, "Can't add a bot", "Bots can't receive DMs."),
        )
        .await;
    }

    let pool = &ctx.data().pool;
    let added = sqlx::query(
        "INSERT INTO moderators (guild_id, user_id, added_by) VALUES ($1, $2, $3)
        ON CONFLICT (guild_id, user_id) DO NOTHING",
    )
    .bind(guild_id.get() as i64)
    .bind(user.id.get() as i64)
    .bind(ctx.author().id.get() as i64)
    .execute(pool)
    .await?
    .rows_affected()
        > 0;

    if !added {
        return reply(
            ctx,
            notice(
                COLOR_INFO,
                "Already a moderator",
                format!("{} is already on the moderator list.", user.mention()),
            ),
        )
        .await;
    }

    // Doubles as a check that this user can actually receive DMs from the bot.
    let guild = verification::guild_name(pool, guild_id.get() as i64).await;
    let welcome = notice(
        COLOR_INFO,
        "You're now a verification moderator",
        format!("New student verification requests for **{guild}** will be sent here."),
    );
    let dm = user
        .id
        .dm(ctx.http(), serenity::CreateMessage::new().embed(welcome))
        .await;

    let mut embed = notice(
        COLOR_SUCCESS,
        "Moderator added",
        format!(
            "{} will be DMed when a new verification request comes in.",
            user.mention()
        ),
    );
    if dm.is_err() {
        embed = embed.field(
            "⚠️ Couldn't send a DM",
            "This user needs to allow DMs from this server to receive requests.",
            false,
        );
    }
    reply(ctx, embed).await
}

/// Stop DMing this user about new verification requests
#[poise::command(slash_command, guild_only, rename = "remove")]
async fn moderator_remove(
    ctx: Context<'_>,
    #[description = "Moderator to remove"] user: serenity::User,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };

    let removed = sqlx::query("DELETE FROM moderators WHERE guild_id = $1 AND user_id = $2")
        .bind(guild_id.get() as i64)
        .bind(user.id.get() as i64)
        .execute(&ctx.data().pool)
        .await?
        .rows_affected()
        > 0;

    let embed = if removed {
        notice(
            COLOR_SUCCESS,
            "Moderator removed",
            format!(
                "{} will no longer be DMed about new requests.",
                user.mention()
            ),
        )
    } else {
        notice(
            COLOR_INFO,
            "Not a moderator",
            format!("{} isn't on the moderator list.", user.mention()),
        )
    };
    reply(ctx, embed).await
}

/// Show who is DMed about new verification requests
#[poise::command(slash_command, guild_only, rename = "list")]
async fn moderator_list(ctx: Context<'_>) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };

    let moderators: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM moderators WHERE guild_id = $1 ORDER BY created_at",
    )
    .bind(guild_id.get() as i64)
    .fetch_all(&ctx.data().pool)
    .await?;

    let description = if moderators.is_empty() {
        "No moderators yet. Add one with `/setup moderator add`.".to_owned()
    } else {
        moderators
            .iter()
            .map(|id| format!("<@{id}>"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    reply(
        ctx,
        notice(
            COLOR_INFO,
            &format!("Verification Moderators ({})", moderators.len()),
            description,
        ),
    )
    .await
}
