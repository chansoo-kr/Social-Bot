//! Named message templates a server can store and reuse: `/util embed ...`.
//!
//! A stored message may contain `{placeholders}`, which resolve on their own when it is sent,
//! and `[[fields]]`, which are asked for in a modal each time it is sent.

use crate::{
    Context, Data, Error,
    commands::verification::{COLOR_DANGER, COLOR_INFO, COLOR_SUCCESS, notice, reply},
    i18n::{self, Lang, t},
    placeholder::{Placeholders, Scope},
    template,
};
use poise::serenity_prelude::{self as serenity, Mentionable};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;

/// Where moderators can design an embed and export it as JSON.
const EDITOR_URL: &str = "https://message.style/app/editor";

/// Discord caps a whole message payload well below this, so anything bigger can't be a
/// real embed and isn't worth downloading.
const MAX_JSON_BYTES: u32 = 256 * 1024;

/// Short enough that `embed:send:<channel id>:<name>` fits in a 100-character custom ID.
const MAX_NAME_CHARS: usize = 64;

/// Discord shows at most this many autocomplete choices.
const AUTOCOMPLETE_LIMIT: i64 = 25;

/// Prefix of the modal that fills in a template's fields; the rest is `<channel id>:<name>`.
const SEND_MODAL_ID: &str = "embed:send:";

/// Discord's limit for modal titles and input labels.
const MAX_LABEL_CHARS: usize = 45;

#[poise::command(
    slash_command,
    guild_only,
    subcommands("create", "modify", "field", "send", "list", "delete")
)]
pub(super) async fn embed(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

/// Save an embed from a JSON file exported from message.style ({placeholders} and [[fields]] allowed)
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
    let fields = template::fields(&data);

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
                    ) + &fields_summary(lang, &fields),
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
    let fields = template::fields(&data);

    // Field settings for blanks that no longer exist are dropped along the way.
    let updated = sqlx::query(
        "UPDATE embeds
        SET data = $3, updated_by = $4,
            fields = (SELECT coalesce(jsonb_object_agg(key, value), '{}'::jsonb)
                      FROM jsonb_each(fields) WHERE key = ANY($5))
        WHERE guild_id = $1 AND name = $2",
    )
    .bind(guild_id.get() as i64)
    .bind(name)
    .bind(sqlx::types::Json(&data))
    .bind(ctx.author().id.get() as i64)
    .bind(&fields)
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
            ) + &fields_summary(lang, &fields),
        ),
    )
    .await
}

/// Choose how a [[field]] of an embed is asked for when it is sent
#[poise::command(slash_command, guild_only)]
pub(super) async fn field(
    ctx: Context<'_>,
    #[description = "Name of the embed"]
    #[description_localized("ko", "임베드 이름")]
    #[autocomplete = "autocomplete_name"]
    name: String,
    #[description = "The field, as written in the embed without the brackets"]
    #[description_localized("ko", "임베드에 쓴 필드 이름 (괄호 제외)")]
    field: String,
    #[description = "Label shown above the input (max 45 characters)"]
    #[description_localized("ko", "입력란 위에 표시할 라벨 (최대 45자)")]
    label: Option<String>,
    #[description = "Allow several lines of text"]
    #[description_localized("ko", "여러 줄 입력 허용")]
    multiline: Option<bool>,
    #[description = "Whether the field must be filled in (default: yes)"]
    #[description_localized("ko", "필수 입력 여부 (기본값: 예)")]
    required: Option<bool>,
    #[description = "Hint shown inside the empty input"]
    #[description_localized("ko", "빈 입력란 안에 보이는 힌트")]
    hint: Option<String>,
) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let pool = &ctx.data().pool;
    let name = name.trim();
    let field = field
        .trim()
        .trim_start_matches(template::OPEN)
        .trim_end_matches(template::CLOSE);

    if label
        .as_ref()
        .is_some_and(|l| l.chars().count() > MAX_LABEL_CHARS)
    {
        return reply(
            ctx,
            notice(
                COLOR_DANGER,
                t!(lang, "Label too long", "라벨이 너무 김"),
                t!(
                    lang,
                    format!("The label must be at most {MAX_LABEL_CHARS} characters."),
                    format!("라벨은 최대 {MAX_LABEL_CHARS}자까지만 허용됩니다."),
                ),
            ),
        )
        .await;
    }

    let Some(stored) = load(pool, guild_id, name).await? else {
        return reply(ctx, not_found(lang, name)).await;
    };
    let fields = template::fields(&stored.data);
    if !fields.iter().any(|f| f == field) {
        return reply(
            ctx,
            notice(
                COLOR_DANGER,
                t!(lang, "Unknown field", "알 수 없는 필드"),
                t!(
                    lang,
                    format!("`{name}` has no `[[{field}]]`."),
                    format!("`{name}`에는 `[[{field}]]`이(가) 없습니다."),
                ) + &fields_summary(lang, &fields),
            ),
        )
        .await;
    }

    let mut config = stored.fields.get(field).cloned().unwrap_or_default();
    if let Some(label) = label {
        config.label = Some(label.trim().to_owned()).filter(|l| !l.is_empty());
    }
    if let Some(multiline) = multiline {
        config.multiline = multiline;
    }
    if let Some(required) = required {
        config.required = required;
    }
    if let Some(hint) = hint {
        config.hint = Some(hint.trim().to_owned()).filter(|h| !h.is_empty());
    }

    sqlx::query(
        "UPDATE embeds SET fields = fields || jsonb_build_object($3::text, $4::jsonb), updated_by = $5
        WHERE guild_id = $1 AND name = $2",
    )
    .bind(guild_id.get() as i64)
    .bind(name)
    .bind(field)
    .bind(sqlx::types::Json(&config))
    .bind(ctx.author().id.get() as i64)
    .execute(pool)
    .await?;

    reply(
        ctx,
        notice(
            COLOR_SUCCESS,
            t!(lang, "Field updated", "필드 설정 완료"),
            t!(
                lang,
                format!("`[[{field}]]` of `{name}` will be asked for like this:\n"),
                format!("`{name}`의 `[[{field}]]`은(는) 이렇게 입력받습니다:\n"),
            ) + &describe_field(lang, field, &config),
        ),
    )
    .await
}

/// Post a saved embed to a channel (fields are asked for in a form first)
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
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    let pool = &ctx.data().pool;
    let name = name.trim();
    let channel_id = channel.map_or(ctx.channel_id(), |channel| channel.id.widen());

    // A modal must be the very first response, so nothing is deferred until we know whether
    // one is needed.
    let Some(stored) = load(pool, guild_id, name).await? else {
        ctx.defer_ephemeral().await?;
        return reply(ctx, not_found(lang, name)).await;
    };
    let fields = template::fields(&stored.data);

    if !fields.is_empty() {
        let poise::Context::Application(app) = ctx else {
            return Ok(());
        };
        let modal = fill_modal(lang, name, channel_id, &fields, &stored.fields);
        app.interaction
            .create_response(
                ctx.http(),
                serenity::CreateInteractionResponse::Modal(modal),
            )
            .await?;
        return Ok(());
    }

    ctx.defer_ephemeral().await?;
    let outcome = deliver(
        &ctx.data().pool,
        ctx.cache(),
        ctx.http(),
        stored.data,
        &HashMap::new(),
        &Scope::of(ctx),
        channel_id,
    )
    .await;
    reply(ctx, outcome_notice(lang, name, channel_id, outcome)).await
}

/// List the embeds saved in this server
#[poise::command(slash_command, guild_only)]
pub(super) async fn list(ctx: Context<'_>) -> Result<(), Error> {
    let lang = i18n::of(ctx);
    ctx.defer_ephemeral().await?;

    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };

    let rows: Vec<(
        String,
        sqlx::types::Json<serde_json::Value>,
        Option<i64>,
        i64,
    )> = sqlx::query_as(
        "SELECT name, data, updated_by, extract(epoch FROM updated_at)::bigint
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
    for (name, data, updated_by, updated_at) in &rows {
        let editor = updated_by.map_or_else(
            || t!(lang, "unknown", "알 수 없음").to_owned(),
            |id| serenity::UserId::new(id as u64).mention().to_string(),
        );
        let _ = write!(lines, "`{name}` · {editor} · <t:{updated_at}:R>");
        let fields = template::fields(&data.0);
        if !fields.is_empty() {
            let _ = write!(lines, " · {}", fields_inline(&fields));
        }
        lines.push('\n');
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
// The fill-in modal
// ---------------------------------------------------------------------------

/// How one `[[field]]` is presented in the send modal. Missing settings fall back to the field
/// name as label, single line, required, no hint.
#[derive(Clone, Default, Serialize, Deserialize)]
struct FieldConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default)]
    multiline: bool,
    #[serde(default = "default_true")]
    required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

fn default_true() -> bool {
    true
}

struct Stored {
    data: serde_json::Value,
    fields: HashMap<String, FieldConfig>,
}

async fn load(
    pool: &sqlx::PgPool,
    guild_id: serenity::GuildId,
    name: &str,
) -> Result<Option<Stored>, Error> {
    type Row = (
        sqlx::types::Json<serde_json::Value>,
        sqlx::types::Json<HashMap<String, FieldConfig>>,
    );
    let row: Option<Row> =
        sqlx::query_as("SELECT data, fields FROM embeds WHERE guild_id = $1 AND name = $2")
            .bind(guild_id.get() as i64)
            .bind(name)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(data, fields)| Stored {
        data: data.0,
        fields: fields.0,
    }))
}

fn fill_modal<'a>(
    lang: Lang,
    name: &str,
    channel_id: serenity::GenericChannelId,
    fields: &'a [String],
    configs: &'a HashMap<String, FieldConfig>,
) -> serenity::CreateModal<'a> {
    let components = fields
        .iter()
        .map(|field| {
            let config = configs.get(field).cloned().unwrap_or_default();
            let style = if config.multiline {
                serenity::InputTextStyle::Paragraph
            } else {
                serenity::InputTextStyle::Short
            };
            let mut input =
                serenity::CreateInputText::new(style, field.clone()).required(config.required);
            if let Some(hint) = config.hint {
                input = input.placeholder(hint);
            }
            let label = config
                .label
                .unwrap_or_else(|| truncate(field, MAX_LABEL_CHARS));
            serenity::CreateModalComponent::Label(
                serenity::CreateLabel::input_text(label, input).description(format!(
                    "{}{field}{}",
                    template::OPEN,
                    template::CLOSE
                )),
            )
        })
        .collect::<Vec<_>>();

    let title = t!(lang, format!("Send: {name}"), format!("전송: {name}"));
    serenity::CreateModal::new(
        format!("{SEND_MODAL_ID}{channel_id}:{name}"),
        truncate(&title, MAX_LABEL_CHARS),
    )
    .components(components)
}

/// Routes the submitted fill-in modal. Called from the gateway for every `embed:` interaction.
pub async fn handle_interaction(
    ctx: &serenity::Context,
    interaction: &serenity::Interaction,
) -> Result<(), Error> {
    let serenity::Interaction::Modal(modal) = interaction else {
        return Ok(());
    };
    let Some(target) = modal.data.custom_id.strip_prefix(SEND_MODAL_ID) else {
        return Ok(());
    };
    let Some(guild_id) = modal.guild_id else {
        return Ok(());
    };
    let lang = Lang::from_locale(&modal.locale);
    let Some((channel_id, name)) = target.split_once(':') else {
        return Ok(());
    };
    let Ok(channel_id) = channel_id.parse::<u64>() else {
        return Ok(());
    };
    let channel_id = serenity::GenericChannelId::new(channel_id);

    // Sending may take longer than Discord's three-second window.
    modal
        .create_response(
            &ctx.http,
            serenity::CreateInteractionResponse::Defer(
                serenity::CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;

    let pool = &ctx.data_ref::<Data>().pool;
    let embed = match load(pool, guild_id, name).await? {
        Some(stored) => {
            let mut values = HashMap::new();
            for component in &modal.data.components {
                let serenity::ModalComponent::Label(label) = component else {
                    continue;
                };
                if let serenity::LabelComponent::InputText(input) = &label.component {
                    values.insert(input.custom_id.to_string(), input.value.to_string());
                }
            }
            let scope = Scope {
                guild_id: Some(guild_id),
                user: Some(&modal.user),
                channel_id: Some(channel_id),
            };
            let outcome = deliver(
                pool,
                &ctx.cache,
                &ctx.http,
                stored.data,
                &values,
                &scope,
                channel_id,
            )
            .await;
            outcome_notice(lang, name, channel_id, outcome)
        }
        None => not_found(lang, name),
    };

    modal
        .create_followup(
            &ctx.http,
            serenity::CreateInteractionResponseFollowup::new()
                .embed(embed)
                .ephemeral(true),
        )
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Rendering and sending
// ---------------------------------------------------------------------------

/// Why a stored embed could not be posted.
enum DeliveryError {
    /// The stored JSON can't become a message; the text says what is wrong.
    Invalid(String),
    /// Discord refused it; the text is Discord's reason.
    Rejected(String),
}

/// Fills in the fields, resolves placeholders and posts the result. Fields go first so a value
/// typed into the form may itself contain a placeholder; optional fields left blank vanish.
async fn deliver(
    pool: &sqlx::PgPool,
    cache: &serenity::Cache,
    http: &serenity::Http,
    mut data: serde_json::Value,
    field_values: &HashMap<String, String>,
    scope: &Scope<'_>,
    channel_id: serenity::GenericChannelId,
) -> Result<(), DeliveryError> {
    template::fill(&mut data, |field| {
        Some(field_values.get(field).cloned().unwrap_or_default())
    });
    Placeholders::load(pool, cache, http, scope)
        .await
        .apply_json(&mut data);

    let message = build_message(data).map_err(DeliveryError::Invalid)?;
    channel_id
        .send_message(http, message)
        .await
        .map_err(|error| DeliveryError::Rejected(error.to_string()))?;
    Ok(())
}

fn outcome_notice(
    lang: Lang,
    name: &str,
    channel_id: serenity::GenericChannelId,
    outcome: Result<(), DeliveryError>,
) -> serenity::CreateEmbed<'static> {
    let channel = channel_id.mention();
    match outcome {
        Ok(()) => notice(
            COLOR_SUCCESS,
            t!(lang, "Embed sent", "임베드 전송 완료"),
            t!(
                lang,
                format!("`{name}` has been posted in {channel}."),
                format!("`{name}`을(를) {channel} 채널에 게시했습니다."),
            ),
        ),
        Err(DeliveryError::Invalid(error)) => notice(
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
        Err(DeliveryError::Rejected(error)) => notice(
            COLOR_DANGER,
            t!(lang, "Could not send", "전송 실패"),
            t!(
                lang,
                format!("Discord rejected the message for {channel}: {error}"),
                format!("Discord가 {channel} 채널로의 메시지를 거부했습니다: {error}"),
            ),
        ),
    }
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
        partial
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
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

/// Trims the name and rejects empty, overlong or unroutable ones with an ephemeral notice. A
/// colon is refused because the send modal's ID uses it as a separator.
async fn valid_name<'a>(
    ctx: Context<'_>,
    lang: Lang,
    name: &'a str,
) -> Result<Option<&'a str>, Error> {
    let name = name.trim();
    if !name.is_empty() && name.chars().count() <= MAX_NAME_CHARS && !name.contains(':') {
        return Ok(Some(name));
    }
    reply(
        ctx,
        notice(
            COLOR_DANGER,
            t!(lang, "Invalid name", "잘못된 이름"),
            t!(
                lang,
                format!(
                    "The name must be between 1 and {MAX_NAME_CHARS} characters and may not \
                     contain `:`."
                ),
                format!("이름은 1자 이상 {MAX_NAME_CHARS}자 이하이며 `:`을 포함할 수 없습니다."),
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
            format!(
                "이 서버에 이름이 `{name}`인 임베드가 없습니다. `/util embed list`를 확인하세요."
            ),
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
                 3. Run `/util embed create` again with `name: {name}` and attach the file\n\n\
                 Write `{{server}}`, `{{member_count}}` or a custom placeholder anywhere to have \
                 it filled in automatically, and `[[question]]` to be asked for a value in a \
                 form every time you send it (up to {} per embed).",
                template::MAX_FIELDS
            ),
            format!(
                "1. {EDITOR_URL} 에서 임베드를 디자인하세요\n\
                 2. JSON 파일로 내보내세요\n\
                 3. `/util embed create`를 `name: {name}`과 파일을 첨부해 다시 실행하세요\n\n\
                 `{{server}}`, `{{member_count}}` 또는 커스텀 플레이스홀더를 쓰면 자동으로 \
                 채워지고, `[[질문]]`을 쓰면 전송할 때마다 폼으로 값을 입력받습니다 (임베드당 \
                 최대 {}개).",
                template::MAX_FIELDS
            ),
        ),
    )
}

/// A line listing the embed's fields, or nothing when it has none.
fn fields_summary(lang: Lang, fields: &[String]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    t!(
        lang,
        format!(
            "\n\nFields asked for when sending: {}\nAdjust each with `/util embed field`.",
            fields_inline(fields)
        ),
        format!(
            "\n\n전송 시 입력받는 필드: {}\n각 필드는 `/util embed field`로 설정할 수 있습니다.",
            fields_inline(fields)
        ),
    )
}

fn fields_inline(fields: &[String]) -> String {
    fields
        .iter()
        .map(|f| format!("`{}{f}{}`", template::OPEN, template::CLOSE))
        .collect::<Vec<_>>()
        .join(" ")
}

fn describe_field(lang: Lang, field: &str, config: &FieldConfig) -> String {
    let yes_no = |b: bool| {
        t!(
            lang,
            if b { "yes" } else { "no" },
            if b { "예" } else { "아니오" }
        )
    };
    t!(
        lang,
        format!(
            "Label: {}\nMultiline: {}\nRequired: {}\nHint: {}",
            config.label.as_deref().unwrap_or(field),
            yes_no(config.multiline),
            yes_no(config.required),
            config.hint.as_deref().unwrap_or("-"),
        ),
        format!(
            "라벨: {}\n여러 줄: {}\n필수: {}\n힌트: {}",
            config.label.as_deref().unwrap_or(field),
            yes_no(config.multiline),
            yes_no(config.required),
            config.hint.as_deref().unwrap_or("-"),
        ),
    )
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max_chars - 1).collect();
    cut.push('…');
    cut
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
                    format!(
                        "파일은 최대 {} KiB까지만 허용됩니다.",
                        MAX_JSON_BYTES / 1024
                    ),
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

    // Fields must fit in one modal and be usable as input IDs.
    let fields = template::fields(&data);
    let invalid: Vec<_> = fields
        .iter()
        .filter(|f| !template::is_valid_field_name(f))
        .collect();
    if !invalid.is_empty() {
        let shown = fields_inline(&invalid.iter().map(|f| (*f).clone()).collect::<Vec<_>>());
        reply(
            ctx,
            rejected(
                t!(lang, "Invalid field name", "잘못된 필드 이름"),
                t!(
                    lang,
                    format!(
                        "Field names may only contain `a-z`, `0-9` and `_` (max {} characters): {shown}",
                        template::MAX_FIELD_NAME_CHARS
                    ),
                    format!(
                        "필드 이름은 `a-z`, `0-9`, `_`만 사용할 수 있습니다 (최대 {}자): {shown}",
                        template::MAX_FIELD_NAME_CHARS
                    ),
                ),
            ),
        )
        .await?;
        return Ok(None);
    }
    if fields.len() > template::MAX_FIELDS {
        reply(
            ctx,
            rejected(
                t!(lang, "Too many fields", "필드가 너무 많음"),
                t!(
                    lang,
                    format!(
                        "A form can ask for at most {} fields, but this embed has {}: {}",
                        template::MAX_FIELDS,
                        fields.len(),
                        fields_inline(&fields)
                    ),
                    format!(
                        "폼은 최대 {}개 필드만 입력받을 수 있지만 이 임베드에는 {}개가 있습니다: {}",
                        template::MAX_FIELDS,
                        fields.len(),
                        fields_inline(&fields)
                    ),
                ),
            ),
        )
        .await?;
        return Ok(None);
    }
    Ok(Some(data))
}
