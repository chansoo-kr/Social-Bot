//! Custom `{placeholders}` a server can define and reuse everywhere: `/util placeholder ...`.

use crate::{
    Context, Error,
    commands::verification::{COLOR_DANGER, COLOR_INFO, COLOR_SUCCESS, notice, reply},
    i18n::{self, t},
    placeholder::{self, MAX_NAME_CHARS, MAX_VALUE_CHARS, Placeholders, Scope},
};
use poise::serenity_prelude as serenity;
use std::fmt::Write as _;

const AUTOCOMPLETE_LIMIT: i64 = 25;

#[poise::command(slash_command, guild_only, subcommands("set", "delete", "list"))]
pub(super) async fn placeholder(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Create a placeholder or change its value everywhere it is used
#[poise::command(slash_command, guild_only)]
pub(super) async fn set(
    ctx: Context<'_>,
    #[description = "Name, used as {name} (a-z, 0-9, _)"]
    #[description_localized("ko", "이름, {이름} 형태로 사용 (a-z, 0-9, _)")]
    #[autocomplete = "autocomplete_name"]
    name: String,
    #[description = "Text the placeholder expands to"]
    #[description_localized("ko", "플레이스홀더가 치환될 텍스트")]
    value: String,
) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };

    let name = name.trim().trim_start_matches('{').trim_end_matches('}');
    if !placeholder::is_valid_custom_name(name) {
        let why = if placeholder::is_default(name) {
            t!(
                lang,
                format!("`{{{name}}}` is a built-in placeholder and cannot be overridden."),
                format!("`{{{name}}}`은(는) 기본 플레이스홀더이므로 덮어쓸 수 없습니다."),
            )
        } else {
            t!(
                lang,
                format!(
                    "Names may only contain `a-z`, `0-9` and `_`, and be at most \
                     {MAX_NAME_CHARS} characters."
                ),
                format!("이름은 `a-z`, `0-9`, `_`만 사용할 수 있고 최대 {MAX_NAME_CHARS}자입니다."),
            )
        };
        return reply(
            ctx,
            notice(COLOR_DANGER, t!(lang, "Invalid name", "잘못된 이름"), why),
        )
        .await;
    }

    if value.chars().count() > MAX_VALUE_CHARS {
        return reply(
            ctx,
            notice(
                COLOR_DANGER,
                t!(lang, "Value too long", "값이 너무 김"),
                t!(
                    lang,
                    format!("The value must be at most {MAX_VALUE_CHARS} characters."),
                    format!("값은 최대 {MAX_VALUE_CHARS}자까지만 허용됩니다."),
                ),
            ),
        )
        .await;
    }

    let previous: Option<String> = sqlx::query_scalar(
        "INSERT INTO custom_placeholders (guild_id, name, value, created_by, updated_by)
        VALUES ($1, $2, $3, $4, $4)
        ON CONFLICT (guild_id, name) DO UPDATE
            SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by
        RETURNING (SELECT value FROM custom_placeholders
                   WHERE guild_id = $1 AND name = $2)",
    )
    .bind(guild_id.get() as i64)
    .bind(name)
    .bind(&value)
    .bind(ctx.author().id.get() as i64)
    .fetch_one(&ctx.data().pool)
    .await?;

    let description = match previous {
        Some(old) if old != value => t!(
            lang,
            format!(
                "`{{{name}}}` now expands to:\n{value}\n\n\
                 Previously:\n{old}\n\n\
                 Everything that uses `{{{name}}}` will show the new value from now on."
            ),
            format!(
                "`{{{name}}}`은(는) 이제 다음으로 치환됩니다:\n{value}\n\n\
                 이전 값:\n{old}\n\n\
                 `{{{name}}}`을(를) 사용하는 모든 곳에 지금부터 새 값이 적용됩니다."
            ),
        ),
        Some(_) => t!(
            lang,
            format!("`{{{name}}}` already had that value."),
            format!("`{{{name}}}`은(는) 이미 그 값이었습니다."),
        ),
        None => t!(
            lang,
            format!(
                "`{{{name}}}` expands to:\n{value}\n\n\
                 Use `{{{name}}}` in any embed to insert it."
            ),
            format!(
                "`{{{name}}}`은(는) 다음으로 치환됩니다:\n{value}\n\n\
                 임베드 어디에서든 `{{{name}}}`을(를) 넣어 사용하세요."
            ),
        ),
    };
    reply(
        ctx,
        notice(
            COLOR_SUCCESS,
            t!(lang, "Placeholder saved", "플레이스홀더 저장 완료"),
            description,
        ),
    )
    .await
}

/// Delete a custom placeholder
#[poise::command(slash_command, guild_only)]
pub(super) async fn delete(
    ctx: Context<'_>,
    #[description = "Name of the placeholder"]
    #[description_localized("ko", "플레이스홀더 이름")]
    #[autocomplete = "autocomplete_name"]
    name: String,
) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let name = name.trim().trim_start_matches('{').trim_end_matches('}');

    let deleted = sqlx::query("DELETE FROM custom_placeholders WHERE guild_id = $1 AND name = $2")
        .bind(guild_id.get() as i64)
        .bind(name)
        .execute(&ctx.data().pool)
        .await?;

    if deleted.rows_affected() == 0 {
        return reply(
            ctx,
            notice(
                COLOR_DANGER,
                t!(lang, "Placeholder not found", "플레이스홀더를 찾을 수 없음"),
                t!(
                    lang,
                    format!("There is no custom placeholder named `{{{name}}}` in this server."),
                    format!("이 서버에 `{{{name}}}`이라는 커스텀 플레이스홀더가 없습니다."),
                ),
            ),
        )
        .await;
    }
    reply(
        ctx,
        notice(
            COLOR_SUCCESS,
            t!(lang, "Placeholder deleted", "플레이스홀더 삭제 완료"),
            t!(
                lang,
                format!(
                    "`{{{name}}}` has been deleted. Text that still contains it will show it \
                     literally."
                ),
                format!(
                    "`{{{name}}}`을(를) 삭제했습니다. 아직 이를 포함한 텍스트에는 그대로 \
                     표시됩니다."
                ),
            ),
        ),
    )
    .await
}

/// Show every placeholder and what it currently expands to
#[poise::command(slash_command, guild_only)]
pub(super) async fn list(ctx: Context<'_>) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let pool = &ctx.data().pool;

    let custom: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, value FROM custom_placeholders WHERE guild_id = $1 ORDER BY name",
    )
    .bind(guild_id.get() as i64)
    .fetch_all(pool)
    .await?;

    // Resolve as this command's author in this channel, so the user sees real values.
    let resolved = Placeholders::load(pool, ctx.cache(), ctx.http(), &Scope::of(ctx)).await;

    let mut defaults = String::new();
    for (name, description) in placeholder::DEFAULTS {
        let value = resolved.get(name).unwrap_or("?");
        let _ = writeln!(
            defaults,
            "`{{{name}}}` → {value}\n-# {}",
            description.in_lang(lang)
        );
    }

    let custom_text = if custom.is_empty() {
        t!(
            lang,
            "None yet. Add one with `/util placeholder set`.",
            "아직 없습니다. `/util placeholder set`으로 추가하세요.",
        )
        .to_owned()
    } else {
        let mut text = String::new();
        for (name, value) in &custom {
            let _ = writeln!(text, "`{{{name}}}` → {}", preview(value));
        }
        text
    };

    let embed = serenity::CreateEmbed::new()
        .color(COLOR_INFO)
        .title(t!(lang, "Placeholders", "플레이스홀더"))
        .description(t!(
            lang,
            "Write any of these as `{name}` inside an embed and it is replaced when sent.",
            "임베드 안에 `{이름}` 형태로 쓰면 전송할 때 실제 값으로 바뀝니다.",
        ))
        .field(t!(lang, "Built-in", "기본"), defaults, false)
        .field(
            t!(
                lang,
                format!("Custom ({})", custom.len()),
                format!("커스텀 ({}개)", custom.len()),
            ),
            custom_text,
            false,
        );
    reply(ctx, embed).await
}

/// Shortens long values so the list stays within Discord's field limit.
fn preview(value: &str) -> String {
    const LIMIT: usize = 80;
    let single_line = value.replace('\n', " ");
    if single_line.chars().count() <= LIMIT {
        return single_line;
    }
    let mut cut: String = single_line.chars().take(LIMIT).collect();
    cut.push('…');
    cut
}

async fn autocomplete_name<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> serenity::CreateAutocompleteResponse<'a> {
    let Some(guild_id) = ctx.guild_id() else {
        return serenity::CreateAutocompleteResponse::new();
    };

    let partial = partial.trim_start_matches('{');
    let pattern = format!(
        "{}%",
        partial
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM custom_placeholders
        WHERE guild_id = $1 AND name LIKE $2 ORDER BY name LIMIT $3",
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
