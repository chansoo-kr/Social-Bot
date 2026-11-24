//! Named message templates a server can store and reuse: `/util embed ...`.

use crate::{
    Context, Error,
    commands::verification::{COLOR_DANGER, COLOR_INFO, COLOR_SUCCESS, notice, reply},
    i18n::{self, Lang, t},
    placeholder::{Placeholders, Scope},
};
use poise::serenity_prelude::{self as serenity, Mentionable};
use serde::Deserialize;
use std::fmt::Write as _;

/// Where moderators can design an embed and export it as JSON.
const EDITOR_URL: &str = "https://message.style/app/editor";

/// Discord caps a whole message payload well below this, so anything bigger can't be a
/// real embed and isn't worth downloading.
const MAX_JSON_BYTES: u32 = 256 * 1024;

const MAX_NAME_CHARS: usize = 100;

/// Discord shows at most this many autocomplete choices.
const AUTOCOMPLETE_LIMIT: i64 = 25;

#[poise::command(
    slash_command,
    guild_only,
    subcommands("create", "modify", "send", "list", "delete")
)]
pub(super) async fn embed(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

/// Save an embed from a JSON file exported from the message.style editor ({placeholders} allowed)
#[poise::command(slash_command, guild_only)]
pub(super) async fn create(
    ctx: Context<'_>,
    #[description = "Name of the embed"]
    #[description_localized("ko", "임베드 이름")]
    name: String,
    #[description = "JSON file exported from https://message.style/app/editor"]
    #[description_localized("ko", "https://message.style/app/editor 에서 내보낸 JSON 파일")]
    file: Option<serenity::Attachment>,
) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };

    let Some(name) = valid_name(ctx, lang, &name).await? else {
        return Ok(());
    };

    // Without a file there is nothing to save yet; explain how to make one.
    let Some(file) = file else {
        return reply(ctx, how_to_create(lang, name)).await;
    };

    let Some(data) = read_embed_json(ctx, lang, &file).await? else {
        return Ok(());
    };

    let inserted = sqlx::query(
        "INSERT INTO embeds (guild_id, name, data, created_by, updated_by)
        VALUES ($1, $2, $3, $4, $4)",
    )
    .bind(guild_id.get() as i64)
    .bind(name)
    .bind(sqlx::types::Json(&data))
    .bind(ctx.author().id.get() as i64)
    .execute(&ctx.data().pool)
    .await;

    match inserted {
        Ok(_) => {
            reply(
                ctx,
                notice(
                    COLOR_SUCCESS,
                    t!(lang, "Embed saved", "임베드 저장 완료"),
                    t!(
                        lang,
                        format!("The embed `{name}` has been saved."),
                        format!("임베드 `{name}`을(를) 저장했습니다."),
                    ),
                ),
            )
            .await
        }
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            reply(
                ctx,
                notice(
                    COLOR_DANGER,
                    t!(lang, "Name already taken", "이미 사용 중인 이름"),
                    t!(
                        lang,
                        format!("An embed named `{name}` already exists in this server."),
                        format!("이 서버에 이름이 `{name}`인 임베드가 이미 있습니다."),
                    ),
                ),
            )
            .await
        }
        Err(error) => Err(error.into()),
    }
}

/// Replace a saved embed with a new JSON file
#[poise::command(slash_command, guild_only)]
pub(super) async fn modify(
    ctx: Context<'_>,
    #[description = "Name of the embed"]
    #[description_localized("ko", "임베드 이름")]
    #[autocomplete = "autocomplete_name"]
    name: String,
    #[description = "JSON file exported from https://message.style/app/editor"]
    #[description_localized("ko", "https://message.style/app/editor 에서 내보낸 JSON 파일")]
    file: serenity::Attachment,
) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let name = name.trim();

    let Some(data) = read_embed_json(ctx, lang, &file).await? else {
        return Ok(());
    };

    let updated = sqlx::query(
        "UPDATE embeds SET data = $3, updated_by = $4 WHERE guild_id = $1 AND name = $2",
    )
    .bind(guild_id.get() as i64)
    .bind(name)
    .bind(sqlx::types::Json(&data))
    .bind(ctx.author().id.get() as i64)
    .execute(&ctx.data().pool)
    .await?;

    if updated.rows_affected() == 0 {
        return reply(ctx, not_found(lang, name)).await;
    }
    reply(
        ctx,
        notice(
            COLOR_SUCCESS,
            t!(lang, "Embed updated", "임베드 수정 완료"),
            t!(
                lang,
                format!("The embed `{name}` has been updated."),
                format!("임베드 `{name}`을(를) 수정했습니다."),
            ),
        ),
    )
    .await
}

/// Post a saved embed to a channel
#[poise::command(slash_command, guild_only)]
pub(super) async fn send(
    ctx: Context<'_>,
    #[description = "Name of the embed"]
    #[description_localized("ko", "임베드 이름")]
    #[autocomplete = "autocomplete_name"]
    name: String,
    #[description = "Channel to post in (defaults to this channel)"]
    #[description_localized("ko", "게시할 채널 (기본값: 현재 채널)")]
    #[channel_types("Text", "News")]
    channel: Option<serenity::GuildChannel>,
) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let name = name.trim();

    let data: Option<sqlx::types::Json<serde_json::Value>> =
        sqlx::query_scalar("SELECT data FROM embeds WHERE guild_id = $1 AND name = $2")
            .bind(guild_id.get() as i64)
            .bind(name)
            .fetch_optional(&ctx.data().pool)
            .await?;
    let Some(sqlx::types::Json(mut data)) = data else {
        return reply(ctx, not_found(lang, name)).await;
    };

    // Placeholders are resolved now, not when the embed was saved, so `/util placeholder set`
    // changes what every embed shows from then on.
    let pool = &ctx.data().pool;
    Placeholders::load(pool, ctx.cache(), ctx.http(), &Scope::of(ctx))
        .await
        .apply_json(&mut data);

    let message = match build_message(data) {
        Ok(message) => message,
        Err(error) => {
            return reply(
                ctx,
                notice(
                    COLOR_DANGER,
                    t!(lang, "Invalid embed", "잘못된 임베드"),
                    t!(
                        lang,
                        format!(
                            "The saved JSON could not be turned into a message: {error}\n\
                             Fix it with `/util embed modify`."
                        ),
                        format!(
                            "저장된 JSON을 메시지로 변환할 수 없습니다: {error}\n\
                             `/util embed modify`로 수정하세요."
                        ),
                    ),
                ),
            )
            .await;
        }
    };

    let channel_id = channel.map_or(ctx.channel_id(), |channel| channel.id.widen());
    if let Err(error) = channel_id.send_message(ctx.http(), message).await {
        return reply(
            ctx,
            notice(
                COLOR_DANGER,
                t!(lang, "Could not send", "전송 실패"),
                t!(
                    lang,
                    format!(
                        "Discord rejected the message for {}: {error}",
                        channel_id.mention()
                    ),
                    format!(
                        "Discord가 {} 채널로의 메시지를 거부했습니다: {error}",
                        channel_id.mention()
                    ),
                ),
            ),
        )
        .await;
    }

    reply(
        ctx,
        notice(
            COLOR_SUCCESS,
            t!(lang, "Embed sent", "임베드 전송 완료"),
            t!(
                lang,
                format!("`{name}` has been posted in {}.", channel_id.mention()),
                format!("`{name}`을(를) {} 채널에 게시했습니다.", channel_id.mention()),
            ),
        ),
    )
    .await
}

/// List the embeds saved in this server
#[poise::command(slash_command, guild_only)]
pub(super) async fn list(ctx: Context<'_>) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };

    let rows: Vec<(String, Option<i64>, i64)> = sqlx::query_as(
        "SELECT name, updated_by, extract(epoch FROM updated_at)::bigint
        FROM embeds WHERE guild_id = $1 ORDER BY name",
    )
    .bind(guild_id.get() as i64)
    .fetch_all(&ctx.data().pool)
    .await?;

    if rows.is_empty() {
        return reply(
            ctx,
            notice(
                COLOR_INFO,
                t!(lang, "No embeds", "임베드 없음"),
                t!(
                    lang,
                    "This server has no saved embeds yet. Create one with `/util embed create`.",
                    "이 서버에 저장된 임베드가 없습니다. `/util embed create`로 만들어 보세요.",
                ),
            ),
        )
        .await;
    }

    let mut lines = String::new();
    for (name, updated_by, updated_at) in &rows {
        let editor = updated_by.map_or_else(
            || t!(lang, "unknown", "알 수 없음").to_owned(),
            |id| serenity::UserId::new(id as u64).mention().to_string(),
        );
        let _ = writeln!(lines, "`{name}` · {editor} · <t:{updated_at}:R>");
    }

    reply(
        ctx,
        notice(
            COLOR_INFO,
            t!(
                lang,
                format!("Saved embeds ({})", rows.len()),
                format!("저장된 임베드 ({}개)", rows.len()),
            )
            .as_str(),
            lines,
        ),
    )
    .await
}

/// Delete a saved embed
#[poise::command(slash_command, guild_only)]
pub(super) async fn delete(
    ctx: Context<'_>,
    #[description = "Name of the embed"]
    #[description_localized("ko", "임베드 이름")]
    #[autocomplete = "autocomplete_name"]
    name: String,
) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let name = name.trim();

    let deleted = sqlx::query("DELETE FROM embeds WHERE guild_id = $1 AND name = $2")
        .bind(guild_id.get() as i64)
        .bind(name)
        .execute(&ctx.data().pool)
        .await?;

    if deleted.rows_affected() == 0 {
        return reply(ctx, not_found(lang, name)).await;
    }
    reply(
        ctx,
        notice(
            COLOR_SUCCESS,
            t!(lang, "Embed deleted", "임베드 삭제 완료"),
            t!(
                lang,
                format!("The embed `{name}` has been deleted."),
                format!("임베드 `{name}`을(를) 삭제했습니다."),
            ),
        ),
    )
    .await
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Offers saved embed names whose prefix matches what the user has typed so far.
async fn autocomplete_name<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> serenity::CreateAutocompleteResponse<'a> {
    let Some(guild_id) = ctx.guild_id() else {
        return serenity::CreateAutocompleteResponse::new();
    };

    // Escape LIKE wildcards so a literal `%` or `_` in the input matches itself.
    let pattern = format!(
        "{}%",
        partial.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM embeds WHERE guild_id = $1 AND name ILIKE $2 ORDER BY name LIMIT $3",
    )
    .bind(guild_id.get() as i64)
    .bind(pattern)
    .bind(AUTOCOMPLETE_LIMIT)
    .fetch_all(&ctx.data().pool)
    .await
    .unwrap_or_default();

    let choices: Vec<_> = names
        .into_iter()
        .map(serenity::AutocompleteChoice::from)
        .collect();
    serenity::CreateAutocompleteResponse::new().set_choices(choices)
}

/// Trims the name and rejects empty or overlong ones with an ephemeral notice.
async fn valid_name<'a>(
    ctx: Context<'_>,
    lang: Lang,
    name: &'a str,
) -> Result<Option<&'a str>, Error> {
    let name = name.trim();
    if !name.is_empty() && name.chars().count() <= MAX_NAME_CHARS {
        return Ok(Some(name));
    }
    reply(
        ctx,
        notice(
            COLOR_DANGER,
            t!(lang, "Invalid name", "잘못된 이름"),
            t!(
                lang,
                format!("The name must be between 1 and {MAX_NAME_CHARS} characters."),
                format!("이름은 1자 이상 {MAX_NAME_CHARS}자 이하여야 합니다."),
            ),
        ),
    )
    .await?;
    Ok(None)
}

fn not_found(lang: Lang, name: &str) -> serenity::CreateEmbed<'static> {
    notice(
        COLOR_DANGER,
        t!(lang, "Embed not found", "임베드를 찾을 수 없음"),
        t!(
            lang,
            format!("There is no embed named `{name}` in this server. See `/util embed list`."),
            format!("이 서버에 이름이 `{name}`인 임베드가 없습니다. `/util embed list`를 확인하세요."),
        ),
    )
}

/// Tells the user to design the embed in the editor and run the command again with the export.
fn how_to_create(lang: Lang, name: &str) -> serenity::CreateEmbed<'static> {
    notice(
        COLOR_INFO,
        t!(lang, "How to create an embed", "임베드 만드는 방법"),
        t!(
            lang,
            format!(
                "1. Design your embed at {EDITOR_URL}\n\
                 2. Export it as a JSON file\n\
                 3. Run `/util embed create` again with `name: {name}` and attach the file"
            ),
            format!(
                "1. {EDITOR_URL} 에서 임베드를 디자인하세요\n\
                 2. JSON 파일로 내보내세요\n\
                 3. `/util embed create`를 `name: {name}`과 파일을 첨부해 다시 실행하세요"
            ),
        ),
    )
}

/// The parts of a message.style export the bot can send. Everything else (webhook name,
/// avatar, components) is ignored.
#[derive(Deserialize)]
struct MessageTemplate {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    embeds: Vec<serenity::Embed>,
}

/// Turns a stored export into something Discord will accept. Mentions are never allowed to
/// ping, since the author of the template may not be the one sending it.
fn build_message(data: serde_json::Value) -> Result<serenity::CreateMessage<'static>, String> {
    let template: MessageTemplate = serde_json::from_value(data).map_err(|e| e.to_string())?;

    let content = template.content.unwrap_or_default();
    if content.is_empty() && template.embeds.is_empty() {
        return Err("it has neither content nor embeds".to_owned());
    }
    if template.embeds.len() > 10 {
        return Err("a message can hold at most 10 embeds".to_owned());
    }

    let embeds: Vec<serenity::CreateEmbed<'static>> =
        template.embeds.into_iter().map(Into::into).collect();
    Ok(serenity::CreateMessage::new()
        .content(content)
        .embeds(embeds)
        .allowed_mentions(serenity::CreateAllowedMentions::new()))
}

/// Downloads and parses the attached JSON. Replies with the problem and returns `None` when the
/// file isn't usable, so the caller can just stop.
async fn read_embed_json(
    ctx: Context<'_>,
    lang: Lang,
    file: &serenity::Attachment,
) -> Result<Option<serde_json::Value>, Error> {
    let rejected = |title: &str, description: String| notice(COLOR_DANGER, title, description);

    if file.size > MAX_JSON_BYTES {
        reply(
            ctx,
            rejected(
                t!(lang, "File too large", "파일이 너무 큼"),
                t!(
                    lang,
                    format!("The file must be at most {} KiB.", MAX_JSON_BYTES / 1024),
                    format!("파일은 최대 {} KiB까지만 허용됩니다.", MAX_JSON_BYTES / 1024),
                ),
            ),
        )
        .await?;
        return Ok(None);
    }

    let bytes = file.download().await?;
    let data = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(data) if data.is_object() => data,
        Ok(_) => {
            reply(
                ctx,
                rejected(
                    t!(lang, "Invalid embed", "잘못된 임베드"),
                    t!(
                        lang,
                        format!("The file must contain a JSON object exported from {EDITOR_URL}."),
                        format!("파일은 {EDITOR_URL} 에서 내보낸 JSON 객체여야 합니다."),
                    ),
                ),
            )
            .await?;
            return Ok(None);
        }
        Err(error) => {
            reply(
                ctx,
                rejected(
                    t!(lang, "Invalid JSON", "잘못된 JSON"),
                    t!(
                        lang,
                        format!("The file could not be parsed as JSON: {error}"),
                        format!("파일을 JSON으로 해석할 수 없습니다: {error}"),
                    ),
                ),
            )
            .await?;
            return Ok(None);
        }
    };

    // Catch broken exports when they are saved rather than when someone tries to send them.
    if let Err(error) = build_message(data.clone()) {
        reply(
            ctx,
            rejected(
                t!(lang, "Invalid embed", "잘못된 임베드"),
                t!(
                    lang,
                    format!("The file is not a valid message export: {error}"),
                    format!("파일이 올바른 메시지 내보내기가 아닙니다: {error}"),
                ),
            ),
        )
        .await?;
        return Ok(None);
    }
    Ok(Some(data))
}
