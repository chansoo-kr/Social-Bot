//! `/setup moderator`: who may manage the bot and who is DMed about new requests.

use crate::{
    Context, Error,
    commands::verification::{self, COLOR_DANGER, COLOR_INFO, COLOR_SUCCESS, notice, reply},
    i18n::{self, t},
};
use poise::serenity_prelude::{self as serenity, Mentionable};

#[poise::command(slash_command, guild_only, subcommands("add", "remove", "list"))]
pub(super) async fn moderator(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// DM this user whenever a new verification request comes in
#[poise::command(
    slash_command,
    guild_only,
    description_localized("ko", "새 인증 신청이 들어올 때마다 이 유저에게 DM을 보냅니다")
)]
async fn add(
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
    description_localized("ko", "이 유저에게 새 인증 신청 DM을 더 이상 보내지 않습니다")
)]
async fn remove(
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
    description_localized("ko", "새 인증 신청 DM을 받는 유저 목록을 봅니다")
)]
async fn list(ctx: Context<'_>) -> Result<(), Error> {
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
