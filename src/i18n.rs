use poise::serenity_prelude as serenity;
use tracing::warn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Ko,
}

impl Lang {
    /// Maps a Discord locale such as `ko` or `en-US` to a supported language.
    /// Anything that isn't Korean falls back to English.
    pub fn from_locale(locale: &str) -> Self {
        if locale == "ko" || locale.starts_with("ko-") {
            Self::Ko
        } else {
            Self::En
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ko => "ko",
        }
    }
}

/// Picks the English or Korean expression for a [`Lang`]. Only the chosen one is evaluated.
macro_rules! t {
    ($lang:expr, $en:expr, $ko:expr $(,)?) => {
        match $lang {
            $crate::i18n::Lang::En => $en,
            $crate::i18n::Lang::Ko => $ko,
        }
    };
}
pub(crate) use t;

/// Language of the user who invoked a command.
pub fn of(ctx: crate::Context<'_>) -> Lang {
    ctx.locale().map_or(Lang::En, Lang::from_locale)
}

/// Records the locale Discord reported for a user so later DMs, which carry no locale of their
/// own, can be written in their language. Runs in the background to keep interactions fast.
pub fn remember(pool: &sqlx::PgPool, user_id: serenity::UserId, locale: &str) {
    let pool = pool.clone();
    let locale = locale.to_owned();
    tokio::spawn(async move {
        let saved = sqlx::query(
            "INSERT INTO user_locales (user_id, locale) VALUES ($1, $2)
            ON CONFLICT (user_id) DO UPDATE SET locale = EXCLUDED.locale
            WHERE user_locales.locale IS DISTINCT FROM EXCLUDED.locale",
        )
        .bind(user_id.get() as i64)
        .bind(locale)
        .execute(&pool)
        .await;
        if let Err(error) = saved {
            warn!("Failed to remember locale of user {user_id}: {error}");
        }
    });
}

/// Stores a best-guess locale for a user the bot hasn't heard from yet. Never overwrites a
/// locale Discord actually reported.
pub async fn seed(pool: &sqlx::PgPool, user_id: serenity::UserId, locale: &str) {
    let seeded = sqlx::query(
        "INSERT INTO user_locales (user_id, locale) VALUES ($1, $2)
        ON CONFLICT (user_id) DO NOTHING",
    )
    .bind(user_id.get() as i64)
    .bind(locale)
    .execute(pool)
    .await;
    if let Err(error) = seeded {
        warn!("Failed to seed locale of user {user_id}: {error}");
    }
}

/// Last known language of a user, or `None` if they have never interacted with the bot.
pub async fn known_lang_of(pool: &sqlx::PgPool, user_id: i64) -> Option<Lang> {
    sqlx::query_scalar::<_, String>("SELECT locale FROM user_locales WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|locale| Lang::from_locale(&locale))
}

/// Last known language of a user, or English if they have never interacted with the bot.
pub async fn lang_of(pool: &sqlx::PgPool, user_id: i64) -> Lang {
    known_lang_of(pool, user_id).await.unwrap_or(Lang::En)
}
