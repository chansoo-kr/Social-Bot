use std::borrow::Cow;

use crate::{Context, Data, Error};
use poise::serenity_prelude::{self as serenity, Mentionable};

pub const BUTTON_ID: &str = "verify:start";
pub const MODAL_ID: &str = "verify:modal";

#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    subcommands("verification")
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
    let modal = serenity::CreateModal::new(MODAL_ID, "Student Verification").components(vec![
        serenity::CreateModalComponent::Label(serenity::CreateLabel::input_text(
            "School",
            serenity::CreateInputText::new(serenity::InputTextStyle::Short, "school"),
        )),
        serenity::CreateModalComponent::Label(serenity::CreateLabel::input_text(
            "Student Number",
            serenity::CreateInputText::new(serenity::InputTextStyle::Short, "student_number"),
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

    sqlx::query(
        "INSERT INTO pending_verifications (guild_id, user_id, school, student_number, proof_url)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (guild_id, user_id) DO UPDATE
        SET school = EXCLUDED.school, student_number = EXCLUDED.student_number,
        proof_url = EXCLUDED.proof_url",
    )
    .bind(guild_id.get() as i64)
    .bind(modal.user.id.get() as i64)
    .bind(school)
    .bind(student_number)
    .bind(proof.url.as_str())
    .execute(&ctx.data_ref::<Data>().pool)
    .await?;

    respond_ephemeral(
        ctx,
        modal,
        "Your verification request has been submitted. Please wait for a moderator to review it.",
    )
    .await?;

    Ok(())
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

#[poise::command(slash_command, guild_only)]
async fn verification(
    ctx: Context<'_>,
    #[description = "Verification channel"] channel: serenity::GuildChannel,
    #[description = "Role to assign after verification"] role: serenity::Role,
) -> Result<(), Error> {
    let embed = serenity::CreateEmbed::new()
        .title("✅ Verification setup complete")
        .color(0x57F287)
        .field("Channel", channel.mention().to_string(), true)
        .field("Role", role.mention().to_string(), true)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "Configured by {}",
            ctx.author().name
        )))
        .timestamp(serenity::Timestamp::now());

    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;

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

    sqlx::query(
        "INSERT INTO verification_settings (guild_id, channel_id, role_id, message_id, updated_by)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (guild_id) DO UPDATE
        SET channel_id = EXCLUDED.channel_id, role_id = EXCLUDED.role_id,
        message_id = EXCLUDED.message_id, updated_by = EXCLUDED.updated_by",
    )
    .bind(channel.base.guild_id.get() as i64)
    .bind(channel.id.get() as i64)
    .bind(role.id.get() as i64)
    .bind(panel.id.get() as i64)
    .bind(ctx.author().id.get() as i64)
    .execute(&ctx.data().pool)
    .await?;

    Ok(())
}
