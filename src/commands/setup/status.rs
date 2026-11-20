//! `/setup status`: the bot's own status message.

use crate::{
    Context, Data, Error,
    commands::verification::{COLOR_DANGER, COLOR_SUCCESS, notice, reply},
    i18n::{self, t},
};
use poise::serenity_prelude as serenity;
use tracing::warn;

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
pub(super) async fn status(
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
