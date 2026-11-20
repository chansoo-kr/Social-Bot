//! The server language and `/setup language`.

use super::verification::{panel_components, panel_embed};
use crate::{
    Context, Error,
    commands::verification::{COLOR_SUCCESS, notice, reply},
    i18n::{self, Lang, t},
};
use poise::serenity_prelude as serenity;

#[derive(Clone, Copy, poise::ChoiceParameter)]
pub(super) enum LanguageChoice {
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

pub(super) async fn store_guild_lang(
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

/// Best guess at a server's language, used when setup isn't told one.
pub(super) async fn detect_guild_lang(ctx: Context<'_>, guild_id: serenity::GuildId) -> Lang {
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

/// Set the language of the bot's public messages in this server
#[poise::command(
    slash_command,
    guild_only,
    description_localized("ko", "이 서버에서 봇의 공개 메시지에 쓰이는 언어를 설정합니다")
)]
pub(super) async fn language(
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
