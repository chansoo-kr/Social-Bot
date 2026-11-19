use std::borrow::Cow;

use crate::{
    Context, Data, Error,
    commands::verification::{
        self, COLOR_DANGER, COLOR_INFO, COLOR_SUCCESS, moderator_only, notice, reply,
    },
    i18n::{self, Lang, t},
};
use poise::serenity_prelude::{self as serenity, Mentionable};
use tracing::warn;

pub const BUTTON_ID: &str = "verify:start";
pub const MODAL_ID: &str = "verify:modal";

/// Bots can't re-upload anything larger than this to a DM.
const MAX_PROOF_BYTES: u32 = 10 * 1024 * 1024;

#[poise::command(
    slash_command,
    guild_only,
    check = "moderator_only",
    subcommands("verification", "moderator", "guild_language", "status")
)]
pub async fn setup(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[derive(Clone, Copy, poise::ChoiceParameter)]
enum LanguageChoice {
    #[name = "English"]
    En,
    #[name = "한국어"]
    Ko,
}

impl From<LanguageChoice> for Lang {
    fn from(choice: LanguageChoice) -> Self {
        match choice {
            LanguageChoice::En => Self::En,
            LanguageChoice::Ko => Self::Ko,
        }
    }
}

pub async fn stored_guild_lang(
    pool: &sqlx::PgPool,
    guild_id: serenity::GuildId,
) -> Result<Option<Lang>, sqlx::Error> {
    let language: Option<String> =
        sqlx::query_scalar("SELECT language FROM guilds WHERE guild_id = $1")
            .bind(guild_id.get() as i64)
            .fetch_optional(pool)
            .await?
            .flatten();
    Ok(language.as_deref().map(Lang::from_locale))
}

async fn store_guild_lang(
    pool: &sqlx::PgPool,
    guild_id: serenity::GuildId,
    lang: Lang,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE guilds SET language = $2 WHERE guild_id = $1")
        .bind(guild_id.get() as i64)
        .bind(lang.code())
        .execute(pool)
        .await?;
    Ok(())
}

fn panel_components(lang: Lang) -> Vec<serenity::CreateComponent<'static>> {
    vec![serenity::CreateComponent::ActionRow(
        serenity::CreateActionRow::buttons(vec![panel_button(lang)]),
    )]
}

// The panel is one public message, so it uses the server's language rather than a viewer's.
pub fn panel_embed(lang: Lang) -> serenity::CreateEmbed<'static> {
    serenity::CreateEmbed::new()
        .title(t!(lang, "Student Verification", "학생 인증"))
        .description(t!(
            lang,
            "Press the button below and fill in the form to get verified.",
            "아래 버튼을 누르고 양식을 작성해 인증을 받으세요."
        ))
        .color(0x5865F2)
}

pub fn panel_button(lang: Lang) -> serenity::CreateButton<'static> {
    serenity::CreateButton::new(BUTTON_ID)
        .label(t!(lang, "Verify", "인증하기"))
        .style(serenity::ButtonStyle::Primary)
}

/// Best guess at a server's language, used when setup isn't told one.
async fn detect_guild_lang(ctx: Context<'_>, guild_id: serenity::GuildId) -> Lang {
    let Ok(guild) = ctx.http().get_guild(guild_id).await else {
        return Lang::En;
    };

    // Only Community servers can change their primary language. Everywhere else it is just
    // Discord's `en-US` default and says nothing about the server.
    let locale = guild.preferred_locale.as_str();
    let is_community = guild.features.iter().any(|f| f.as_str() == "COMMUNITY");
    if is_community || locale != "en-US" {
        return Lang::from_locale(locale);
    }

    // Discord has no API for a user's country or language; it is only revealed when they
    // interact with the bot. So the owner's language is known only if they are the one running
    // this command or have used the bot before.
    if guild.owner_id == ctx.author().id {
        return i18n::of(ctx);
    }
    i18n::known_lang_of(&ctx.data().pool, guild.owner_id.get() as i64)
        .await
        .unwrap_or(Lang::En)
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

pub async fn handle_verification(
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

    verification::announce_request(ctx, &pending, proof).await
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

/// Post the verification panel and choose the role given to verified students
#[poise::command(
    slash_command,
    guild_only,
    description_localized("ko", "인증 패널을 게시하고 인증된 학생에게 부여할 역할을 설정합니다")
)]
async fn verification(
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

/// Set the language of the bot's public messages in this server
#[poise::command(
    slash_command,
    guild_only,
    rename = "language",
    description_localized("ko", "이 서버에서 봇의 공개 메시지에 쓰이는 언어를 설정합니다")
)]
async fn guild_language(
    ctx: Context<'_>,
    #[description = "Language to use"]
    #[description_localized("ko", "사용할 언어")]
    language: LanguageChoice,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let lang = i18n::of(ctx);
    let pool = &ctx.data().pool;
    ctx.defer_ephemeral().await?;

    let guild_lang: Lang = language.into();
    store_guild_lang(pool, guild_id, guild_lang).await?;

    // The panel already posted in the verification channel is rewritten in place, so nobody has
    // to run the full setup again just to change its language.
    let panel = sqlx::query_as::<_, (i64, Option<i64>)>(
        "SELECT channel_id, message_id FROM verification_settings WHERE guild_id = $1",
    )
    .bind(guild_id.get() as i64)
    .fetch_optional(pool)
    .await?;
    let panel_updated = match panel {
        Some((channel_id, Some(message_id))) => Some(
            serenity::GenericChannelId::new(channel_id as u64)
                .edit_message(
                    ctx.http(),
                    serenity::MessageId::new(message_id as u64),
                    serenity::EditMessage::new()
                        .embed(panel_embed(guild_lang))
                        .components(panel_components(guild_lang)),
                )
                .await
                .is_ok(),
        ),
        _ => None,
    };

    let name = t!(guild_lang, "English", "한국어");
    let mut embed = notice(
        COLOR_SUCCESS,
        t!(
            lang,
            "Server language updated",
            "서버 언어가 변경되었습니다"
        ),
        t!(
            lang,
            format!("Public messages in this server will now be in **{name}**."),
            format!("이 서버의 공개 메시지는 이제 **{name}**로 표시됩니다."),
        ),
    )
    .footer(serenity::CreateEmbedFooter::new(t!(
        lang,
        "Replies shown only to one person still follow that person's Discord language.",
        "한 사람에게만 보이는 응답은 계속 그 사람의 Discord 언어를 따릅니다."
    )));
    match panel_updated {
        Some(true) => {
            embed = embed.field(
                t!(lang, "Verification panel", "인증 패널"),
                t!(
                    lang,
                    "Updated to the new language.",
                    "새 언어로 수정했습니다."
                ),
                false,
            );
        }
        Some(false) => {
            embed = embed.field(
                t!(
                    lang,
                    "⚠️ Couldn't update the verification panel",
                    "⚠️ 인증 패널을 수정하지 못했습니다"
                ),
                t!(
                    lang,
                    "It may have been deleted. Run `/setup verification` to post a new one.",
                    "패널이 삭제되었을 수 있습니다. `/setup verification`으로 새로 게시해 주세요."
                ),
                false,
            );
        }
        None => {}
    }
    reply(ctx, embed).await
}

/// The bot has one profile shared by every server it is in, so its status is not a per-server
/// setting: only the bot's owner (the application owner or team) may change it.
async fn bot_owner_only(ctx: Context<'_>) -> Result<bool, Error> {
    if ctx.framework().options().owners.contains(&ctx.author().id) {
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
                "The status is shown in every server the bot is in, so only the bot's owner can \
                 change it.",
                "상태 메시지는 봇이 들어가 있는 모든 서버에 표시되므로 봇 소유자만 변경할 수 \
                 있습니다."
            ),
        ),
    )
    .await?;
    Ok(false)
}

fn apply_status(ctx: &serenity::Context, text: Option<&str>) {
    ctx.set_activity(text.map(serenity::ActivityData::custom));
}

/// Re-applies the saved status after a (re)connect; Discord forgets it when the bot restarts.
pub async fn restore_status(ctx: &serenity::Context) {
    let saved = sqlx::query_scalar::<_, Option<String>>("SELECT status_text FROM bot_settings")
        .fetch_optional(&ctx.data_ref::<Data>().pool)
        .await;
    match saved {
        Ok(text) => {
            if let Some(text) = text.flatten() {
                apply_status(ctx, Some(&text));
            }
        }
        Err(error) => warn!("Failed to load the bot's saved status: {error}"),
    }
}

/// Set the status message shown on the bot's profile (bot owner only)
#[poise::command(
    slash_command,
    guild_only,
    check = "bot_owner_only",
    description_localized("ko", "봇 프로필에 표시되는 상태 메시지를 설정합니다 (봇 소유자 전용)")
)]
async fn status(
    ctx: Context<'_>,
    #[description = "Status text. Leave it out to clear the status"]
    #[description_localized("ko", "상태 메시지. 생략하면 상태 메시지를 지웁니다")]
    #[max_length = 128]
    text: Option<String>,
) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let text = text.as_deref().map(str::trim).filter(|t| !t.is_empty());

    // Saved first so the status survives restarts; `restore_status` reads it back.
    sqlx::query(
        "INSERT INTO bot_settings (status_text, updated_by) VALUES ($1, $2)
        ON CONFLICT (singleton) DO UPDATE
        SET status_text = EXCLUDED.status_text, updated_by = EXCLUDED.updated_by,
        updated_at = now()",
    )
    .bind(text)
    .bind(ctx.author().id.get() as i64)
    .execute(&ctx.data().pool)
    .await?;
    apply_status(ctx.serenity_context(), text);

    let embed = match text {
        Some(text) => notice(
            COLOR_SUCCESS,
            t!(lang, "Status updated", "상태 메시지가 변경되었습니다"),
            text,
        ),
        None => notice(
            COLOR_SUCCESS,
            t!(lang, "Status cleared", "상태 메시지를 지웠습니다"),
            t!(
                lang,
                "The bot no longer shows a status message.",
                "봇이 더 이상 상태 메시지를 표시하지 않습니다."
            ),
        ),
    };
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
#[poise::command(
    slash_command,
    guild_only,
    rename = "add",
    description_localized("ko", "새 인증 신청이 들어올 때마다 이 유저에게 DM을 보냅니다")
)]
async fn moderator_add(
    ctx: Context<'_>,
    #[description = "User to notify about new requests"]
    #[description_localized("ko", "새 신청 알림을 받을 유저")]
    user: serenity::User,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    if user.bot() {
        return reply(
            ctx,
            notice(
                COLOR_DANGER,
                t!(lang, "Can't add a bot", "봇은 추가할 수 없습니다"),
                t!(
                    lang,
                    "Bots can't receive DMs.",
                    "봇은 DM을 받을 수 없습니다."
                ),
            ),
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
                t!(lang, "Already a moderator", "이미 모더레이터입니다"),
                t!(
                    lang,
                    format!("{} is already on the moderator list.", user.mention()),
                    format!("{} 님은 이미 모더레이터 목록에 있습니다.", user.mention()),
                ),
            ),
        )
        .await;
    }

    // Until the new moderator interacts with the bot themselves, assume they read the same
    // language as the person adding them.
    if let Some(locale) = ctx.locale() {
        i18n::seed(pool, user.id, locale).await;
    }
    let their_lang = i18n::lang_of(pool, user.id.get() as i64).await;

    // Doubles as a check that this user can actually receive DMs from the bot.
    let guild = verification::guild_name(pool, guild_id.get() as i64, their_lang).await;
    let welcome = notice(
        COLOR_INFO,
        t!(
            their_lang,
            "You're now a verification moderator",
            "인증 모더레이터로 지정되었습니다"
        ),
        t!(
            their_lang,
            format!("New student verification requests for **{guild}** will be sent here."),
            format!("**{guild}**의 새 학생 인증 신청이 이곳으로 전송됩니다."),
        ),
    );
    let dm = user
        .id
        .dm(ctx.http(), serenity::CreateMessage::new().embed(welcome))
        .await;

    let mut embed = notice(
        COLOR_SUCCESS,
        t!(lang, "Moderator added", "모더레이터 추가됨"),
        t!(
            lang,
            format!(
                "{} will be DMed when a new verification request comes in.",
                user.mention()
            ),
            format!(
                "새 인증 신청이 들어오면 {} 님에게 DM을 보냅니다.",
                user.mention()
            ),
        ),
    );
    if dm.is_err() {
        embed = embed.field(
            t!(lang, "⚠️ Couldn't send a DM", "⚠️ DM을 보내지 못했습니다"),
            t!(
                lang,
                "This user needs to allow DMs from this server to receive requests.",
                "신청을 받으려면 이 유저가 이 서버의 DM을 허용해야 합니다."
            ),
            false,
        );
    }
    reply(ctx, embed).await
}

/// Stop DMing this user about new verification requests
#[poise::command(
    slash_command,
    guild_only,
    rename = "remove",
    description_localized("ko", "이 유저에게 새 인증 신청 DM을 더 이상 보내지 않습니다")
)]
async fn moderator_remove(
    ctx: Context<'_>,
    #[description = "Moderator to remove"]
    #[description_localized("ko", "제거할 모더레이터")]
    user: serenity::User,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let lang = i18n::of(ctx);

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
            t!(lang, "Moderator removed", "모더레이터 제거됨"),
            t!(
                lang,
                format!(
                    "{} will no longer be DMed about new requests.",
                    user.mention()
                ),
                format!(
                    "{} 님에게 더 이상 새 신청 DM을 보내지 않습니다.",
                    user.mention()
                ),
            ),
        )
    } else {
        notice(
            COLOR_INFO,
            t!(lang, "Not a moderator", "모더레이터가 아닙니다"),
            t!(
                lang,
                format!("{} isn't on the moderator list.", user.mention()),
                format!("{} 님은 모더레이터 목록에 없습니다.", user.mention()),
            ),
        )
    };
    reply(ctx, embed).await
}

/// Show who is DMed about new verification requests
#[poise::command(
    slash_command,
    guild_only,
    rename = "list",
    description_localized("ko", "새 인증 신청 DM을 받는 유저 목록을 봅니다")
)]
async fn moderator_list(ctx: Context<'_>) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let lang = i18n::of(ctx);

    let moderators: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM moderators WHERE guild_id = $1 ORDER BY created_at",
    )
    .bind(guild_id.get() as i64)
    .fetch_all(&ctx.data().pool)
    .await?;

    let description = if moderators.is_empty() {
        t!(
            lang,
            "No moderators yet. Add one with `/setup moderator add`.",
            "아직 모더레이터가 없습니다. `/setup moderator add`로 추가해 주세요."
        )
        .to_owned()
    } else {
        moderators
            .iter()
            .map(|id| format!("<@{id}>"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let count = moderators.len();
    reply(
        ctx,
        notice(
            COLOR_INFO,
            &t!(
                lang,
                format!("Verification Moderators ({count})"),
                format!("인증 모더레이터 ({count})"),
            ),
            description,
        ),
    )
    .await
}
