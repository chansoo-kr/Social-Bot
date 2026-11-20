//! `/setup verification` and everything it sets in motion: the settings it stores, the public
//! panel it posts, and the form applicants submit from that panel.

use std::borrow::Cow;

use super::language::{LanguageChoice, detect_guild_lang, store_guild_lang, stored_guild_lang};
use crate::{
    Context, Data, Error,
    commands::verification::{
        COLOR_INFO, COLOR_SUCCESS, announce_request, notice, reply, upsert_pending,
    },
    i18n::{self, Lang, t},
};
use poise::serenity_prelude::{self as serenity, Mentionable};

const BUTTON_ID: &str = "verify:start";
const MODAL_ID: &str = "verify:modal";

/// Bots can't re-upload anything larger than this to a DM.
const MAX_PROOF_BYTES: u32 = 10 * 1024 * 1024;

/// Post the verification panel and choose the role given to verified students
#[poise::command(
    slash_command,
    guild_only,
    description_localized("ko", "인증 패널을 게시하고 인증된 학생에게 부여할 역할을 설정합니다")
)]
pub(super) async fn verification(
    ctx: Context<'_>,
    #[description = "Verification channel"]
    #[description_localized("ko", "인증 채널")]
    channel: serenity::GuildChannel,
    #[description = "Role to assign after verification"]
    #[description_localized("ko", "인증 후 부여할 역할")]
    role: serenity::Role,
    #[description = "Minutes a user must wait between requests (default 10)"]
    #[description_localized("ko", "재신청까지 기다려야 하는 시간(분, 기본 10)")]
    #[min = 0]
    #[max = 10080]
    cooldown_minutes: Option<i32>,
    #[description = "Server language (same as /setup language; detected if never set)"]
    #[description_localized(
        "ko",
        "서버 언어 (/setup language와 동일, 설정한 적이 없으면 자동 감지)"
    )]
    language: Option<LanguageChoice>,
    #[description = "Channel that also receives every request, like the moderator DMs"]
    #[description_localized("ko", "모더레이터 DM처럼 모든 신청이 함께 기록되는 채널")]
    #[channel_types("Text")]
    log_channel: Option<serenity::GuildChannel>,
) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    let pool = &ctx.data().pool;
    let guild_id = channel.base.guild_id;
    ctx.defer_ephemeral().await?;

    // An explicit choice wins, then whatever was set or detected before. Detection only runs
    // while the server has no language yet, so re-running setup never silently flips it.
    let panel_lang = match (language, stored_guild_lang(pool, guild_id).await?) {
        (Some(choice), _) => choice.into(),
        (None, Some(stored)) => stored,
        (None, None) => detect_guild_lang(ctx, guild_id).await,
    };

    let panel = channel
        .send_message(
            ctx.http(),
            serenity::CreateMessage::new()
                .embed(panel_embed(panel_lang))
                .components(panel_components(panel_lang)),
        )
        .await?;

    store_guild_lang(pool, guild_id, panel_lang).await?;

    // 600 mirrors the column default. Omitted options keep whatever was configured before.
    let (cooldown_seconds, log_channel_id) = sqlx::query_as::<_, (i32, Option<i64>)>(
        "INSERT INTO verification_settings
            (guild_id, channel_id, role_id, message_id, updated_by, cooldown_seconds,
            log_channel_id)
        VALUES ($1, $2, $3, $4, $5, COALESCE($6, 600), $7)
        ON CONFLICT (guild_id) DO UPDATE
        SET channel_id = EXCLUDED.channel_id, role_id = EXCLUDED.role_id,
        message_id = EXCLUDED.message_id, updated_by = EXCLUDED.updated_by,
        cooldown_seconds = COALESCE($6, verification_settings.cooldown_seconds),
        log_channel_id = COALESCE($7, verification_settings.log_channel_id)
        RETURNING cooldown_seconds, log_channel_id",
    )
    .bind(guild_id.get() as i64)
    .bind(channel.id.get() as i64)
    .bind(role.id.get() as i64)
    .bind(panel.id.get() as i64)
    .bind(ctx.author().id.get() as i64)
    .bind(cooldown_minutes.map(|minutes| minutes * 60))
    .bind(log_channel.as_ref().map(|channel| channel.id.get() as i64))
    .fetch_one(pool)
    .await?;

    let cooldown_minutes = cooldown_seconds / 60;
    let embed = serenity::CreateEmbed::new()
        .title(t!(
            lang,
            "✅ Verification setup complete",
            "✅ 인증 설정 완료"
        ))
        .color(0x57F287)
        .field(
            t!(lang, "Channel", "채널"),
            channel.mention().to_string(),
            true,
        )
        .field(t!(lang, "Role", "역할"), role.mention().to_string(), true)
        .field(
            t!(lang, "Cooldown", "쿨다운"),
            t!(
                lang,
                format!("{cooldown_minutes} min"),
                format!("{cooldown_minutes}분"),
            ),
            true,
        )
        .field(
            t!(lang, "Server language", "서버 언어"),
            t!(panel_lang, "English", "한국어"),
            true,
        )
        .field(
            t!(lang, "Log channel", "로그 채널"),
            match log_channel_id {
                Some(id) => format!("<#{id}>"),
                None => t!(lang, "Not set", "설정 안 됨").to_owned(),
            },
            true,
        )
        .footer(serenity::CreateEmbedFooter::new(t!(
            lang,
            format!("Configured by {}", ctx.author().name),
            format!("설정한 사람: {}", ctx.author().name),
        )))
        .timestamp(serenity::Timestamp::now());

    reply(ctx, embed).await
}

// ---------------------------------------------------------------------------
// The public panel
// ---------------------------------------------------------------------------

pub(super) fn panel_components(lang: Lang) -> Vec<serenity::CreateComponent<'static>> {
    vec![serenity::CreateComponent::ActionRow(
        serenity::CreateActionRow::buttons(vec![panel_button(lang)]),
    )]
}

// The panel is one public message, so it uses the server's language rather than a viewer's.
pub(super) fn panel_embed(lang: Lang) -> serenity::CreateEmbed<'static> {
    serenity::CreateEmbed::new()
        .title(t!(lang, "Student Verification", "학생 인증"))
        .description(t!(
            lang,
            "Press the button below and fill in the form to get verified.",
            "아래 버튼을 누르고 양식을 작성해 인증을 받으세요."
        ))
        .color(0x5865F2)
}

fn panel_button(lang: Lang) -> serenity::CreateButton<'static> {
    serenity::CreateButton::new(BUTTON_ID)
        .label(t!(lang, "Verify", "인증하기"))
        .style(serenity::ButtonStyle::Primary)
}

// ---------------------------------------------------------------------------
// The applicant's side: the panel button, the form, and its submission
// ---------------------------------------------------------------------------

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
    let lang = Lang::from_locale(&component.locale);

    // Checked again on submit; this just saves the user from filling in a form that will bounce.
    let pool = &ctx.data_ref::<Data>().pool;
    if let Some(until) = cooldown_until(pool, guild_id, component.user.id).await? {
        component
            .create_response(&ctx.http, ephemeral_embed(cooldown_notice(until, lang)))
            .await?;
        return Ok(());
    }

    let modal = serenity::CreateModal::new(MODAL_ID, t!(lang, "Student Verification", "학생 인증"))
        .components(vec![
            serenity::CreateModalComponent::Label(serenity::CreateLabel::input_text(
                t!(lang, "School", "학교"),
                serenity::CreateInputText::new(serenity::InputTextStyle::Short, "school")
                    .max_length(100),
            )),
            serenity::CreateModalComponent::Label(serenity::CreateLabel::input_text(
                t!(lang, "Student Number", "학번"),
                serenity::CreateInputText::new(serenity::InputTextStyle::Short, "student_number")
                    .max_length(32),
            )),
            serenity::CreateModalComponent::Label(
                serenity::CreateLabel::file_upload(
                    t!(lang, "Student ID Photo", "학생증 사진"),
                    serenity::CreateFileUpload::new("proof")
                        .max_values(1)
                        .file_types(vec![Cow::Borrowed("image")]),
                )
                .description(t!(
                    lang,
                    "Please cover any unnecessary personal information before uploading",
                    "업로드 전에 불필요한 개인정보는 가려 주세요"
                )),
            ),
        ]);

    component
        .create_response(&ctx.http, serenity::CreateInteractionResponse::Modal(modal))
        .await?;
    Ok(())
}

async fn handle_verification(
    ctx: &serenity::Context,
    modal: &serenity::ModalInteraction,
) -> Result<(), Error> {
    let Some(guild_id) = modal.guild_id else {
        return Ok(());
    };
    let lang = Lang::from_locale(&modal.locale);

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
        respond_ephemeral(
            ctx,
            modal,
            t!(
                lang,
                "Please fill in every field and attach a photo.",
                "모든 항목을 입력하고 사진을 첨부해 주세요."
            ),
        )
        .await?;
        return Ok(());
    };

    let is_image = proof
        .content_type
        .as_deref()
        .is_some_and(|content_type| content_type.starts_with("image/"));
    if !is_image || proof.size > MAX_PROOF_BYTES {
        respond_ephemeral(
            ctx,
            modal,
            t!(
                lang,
                "The photo must be an image under 10 MB.",
                "사진은 10MB 이하의 이미지여야 합니다."
            ),
        )
        .await?;
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
            t!(
                lang,
                "Verification is not available on this server right now.",
                "현재 이 서버에서는 인증을 이용할 수 없습니다."
            ),
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
            Some(until) => cooldown_notice(until, lang),
            None => notice(
                COLOR_INFO,
                t!(lang, "Please wait", "잠시만 기다려 주세요"),
                t!(
                    lang,
                    "Try again in a moment.",
                    "잠시 후 다시 시도해 주세요."
                ),
            ),
        };
        modal
            .create_response(&ctx.http, ephemeral_embed(embed))
            .await?;
        return Ok(());
    }

    let pending = upsert_pending(
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
                t!(lang, "Request submitted", "신청이 접수되었습니다"),
                t!(
                    lang,
                    "A moderator will review your request. I'll DM you the result if your DMs \
                     are open.",
                    "모더레이터가 신청을 검토할 예정입니다. DM이 열려 있으면 결과를 DM으로 \
                     알려드립니다."
                ),
            )),
        )
        .await?;

    announce_request(ctx, &pending, proof).await
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

fn cooldown_notice(until: i64, lang: Lang) -> serenity::CreateEmbed<'static> {
    notice(
        COLOR_INFO,
        t!(lang, "Please wait", "잠시만 기다려 주세요"),
        t!(
            lang,
            format!("You submitted a request recently. You can submit again <t:{until}:R>."),
            format!("최근에 신청을 제출하셨습니다. <t:{until}:R>에 다시 신청할 수 있습니다."),
        ),
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
