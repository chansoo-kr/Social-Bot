//! `{placeholder}` substitution for any text the bot sends on behalf of a server.
//!
//! Two kinds exist. *Default* placeholders are computed from Discord state at render time
//! (`{member_count}`, `{server}`, `{user}`, ...). *Custom* placeholders are per-server values a
//! moderator sets with `/util placeholder set`; because text is only resolved when it is rendered,
//! changing one changes every embed, message or name that uses it from then on.
//!
//! Unknown placeholders are left untouched so a stray `{}` in prose never disappears.

use crate::i18n::{Lang, t};
use poise::serenity_prelude::{self as serenity, Mentionable};
use std::collections::HashMap;
use tracing::warn;

/// Allowed length of a custom placeholder name.
pub const MAX_NAME_CHARS: usize = 32;
/// Allowed length of a custom placeholder value.
pub const MAX_VALUE_CHARS: usize = 1000;

/// Every default placeholder, with what it expands to. Kept here so `/util placeholder list` and
/// the resolver can never drift apart.
pub const DEFAULTS: &[(&str, Description)] = &[
    ("server", Description::new("Server name", "서버 이름")),
    ("server_id", Description::new("Server ID", "서버 ID")),
    (
        "member_count",
        Description::new("Number of members in the server", "서버 멤버 수"),
    ),
    (
        "user",
        Description::new("Mention of the user", "사용자 멘션"),
    ),
    (
        "user_name",
        Description::new("Display name of the user", "사용자 표시 이름"),
    ),
    ("user_id", Description::new("ID of the user", "사용자 ID")),
    (
        "channel",
        Description::new("Mention of the channel", "채널 멘션"),
    ),
    (
        "now",
        Description::new("Current date and time", "현재 날짜와 시간"),
    ),
];

/// A bilingual one-liner about a default placeholder.
pub struct Description {
    en: &'static str,
    ko: &'static str,
}

impl Description {
    const fn new(en: &'static str, ko: &'static str) -> Self {
        Self { en, ko }
    }

    pub fn in_lang(&self, lang: Lang) -> &'static str {
        t!(lang, self.en, self.ko)
    }
}

/// True for names that may be used as a custom placeholder: `a-z`, `0-9` and `_`, not too long,
/// and not shadowing a default.
pub fn is_valid_custom_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !is_default(name)
}

pub fn is_default(name: &str) -> bool {
    DEFAULTS.iter().any(|(default, _)| *default == name)
}

/// Who and where a piece of text is being rendered for. Everything is optional: a message posted
/// by a schedule has no user, a DM has no channel.
#[derive(Default)]
pub struct Scope<'a> {
    pub guild_id: Option<serenity::GuildId>,
    pub user: Option<&'a serenity::User>,
    pub channel_id: Option<serenity::GenericChannelId>,
}

impl<'a> Scope<'a> {
    /// The scope of a slash command: its server, author and channel.
    pub fn of(ctx: crate::Context<'a>) -> Self {
        Self {
            guild_id: ctx.guild_id(),
            user: Some(ctx.author()),
            channel_id: Some(ctx.channel_id()),
        }
    }
}

/// A snapshot of every placeholder value for one render. Build it once, apply it to as many
/// strings as needed.
pub struct Placeholders {
    values: HashMap<String, String>,
}

impl Placeholders {
    /// Gathers default values from Discord and custom values from the database. Anything that
    /// cannot be fetched is simply absent, so its placeholders stay as written.
    pub async fn load(
        pool: &sqlx::PgPool,
        cache: &serenity::Cache,
        http: &serenity::Http,
        scope: &Scope<'_>,
    ) -> Self {
        let mut values = HashMap::new();

        if let Some(guild_id) = scope.guild_id {
            // Custom values first so a default with the same name (which `is_valid_custom_name`
            // prevents, but old rows might exist) always wins.
            match custom_values(pool, guild_id).await {
                Ok(custom) => values.extend(custom),
                Err(error) => warn!("Failed to load placeholders of guild {guild_id}: {error}"),
            }

            values.insert("server_id".to_owned(), guild_id.to_string());
            if let Some((name, member_count)) = guild_summary(cache, http, guild_id).await {
                values.insert("server".to_owned(), name);
                if let Some(count) = member_count {
                    values.insert("member_count".to_owned(), count.to_string());
                }
            }
        }

        if let Some(user) = scope.user {
            values.insert("user".to_owned(), user.mention().to_string());
            values.insert("user_name".to_owned(), user.display_name().to_owned());
            values.insert("user_id".to_owned(), user.id.to_string());
        }
        if let Some(channel_id) = scope.channel_id {
            values.insert("channel".to_owned(), channel_id.mention().to_string());
        }
        values.insert(
            "now".to_owned(),
            format!("<t:{}:f>", serenity::Timestamp::now().unix_timestamp()),
        );

        Self { values }
    }

    /// The current value of one placeholder, if it has one in this scope.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// Replaces every `{name}` whose value is known. Unknown or malformed ones are kept verbatim.
    pub fn apply(&self, text: &str) -> String {
        crate::template::substitute(text, "{", "}", |name| self.values.get(name).cloned())
    }

    /// Applies [`Self::apply`] to every string inside a JSON document, leaving keys and
    /// non-string values alone. Lets a stored message template use placeholders anywhere.
    pub fn apply_json(&self, value: &mut serde_json::Value) {
        crate::template::map_strings(value, &|text| self.apply(text));
    }
}

async fn custom_values(
    pool: &sqlx::PgPool,
    guild_id: serenity::GuildId,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as("SELECT name, value FROM custom_placeholders WHERE guild_id = $1")
        .bind(guild_id.get() as i64)
        .fetch_all(pool)
        .await
}

/// Name and member count of a server, from the cache when possible and the API otherwise.
async fn guild_summary(
    cache: &serenity::Cache,
    http: &serenity::Http,
    guild_id: serenity::GuildId,
) -> Option<(String, Option<u32>)> {
    if let Some(guild) = cache.guild(guild_id) {
        return Some((guild.name.to_string(), Some(guild.member_count.get())));
    }
    match http.get_guild_with_counts(guild_id).await {
        Ok(guild) => Some((
            guild.name.to_string(),
            guild.approximate_member_count.map(|c| c.get()),
        )),
        Err(error) => {
            warn!("Failed to fetch guild {guild_id} for placeholders: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Placeholders {
        Placeholders {
            values: HashMap::from([
                ("server".to_owned(), "Social".to_owned()),
                ("member_count".to_owned(), "42".to_owned()),
            ]),
        }
    }

    #[test]
    fn replaces_known_and_keeps_unknown() {
        let p = fixture();
        assert_eq!(
            p.apply("Welcome to {server}! {member_count} members, {unknown} {} { x } {server"),
            "Welcome to Social! 42 members, {unknown} {} { x } {server"
        );
    }

    #[test]
    fn walks_json_strings_only() {
        let p = fixture();
        let mut v = serde_json::json!({"{server}": ["{server}", 1, {"a": "{member_count}"}]});
        p.apply_json(&mut v);
        assert_eq!(
            v,
            serde_json::json!({"{server}": ["Social", 1, {"a": "42"}]})
        );
    }

    #[test]
    fn validates_custom_names() {
        assert!(is_valid_custom_name("rules_link"));
        assert!(!is_valid_custom_name("server"));
        assert!(!is_valid_custom_name("Rules"));
        assert!(!is_valid_custom_name(""));
        assert!(!is_valid_custom_name("a-b"));
    }
}
