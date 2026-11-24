use std::sync::Arc;

use anyhow::Context as _;
use poise::serenity_prelude::{self as serenity};
use sqlx::postgres::PgPoolOptions;
use tracing::info;

mod commands;
mod events;
mod i18n;
mod placeholder;
mod template;

struct Data {
    pool: sqlx::Pool<sqlx::Postgres>,
}
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    dotenvy::dotenv().ok();

    info!("Dotenv loaded");

    info!("Connecting to database...");

    let database_url = std::env::var("DATABASE_URL")?;

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .context("Failed to connect to database")?;

    info!("Connected to database");

    info!("Starting bot...");

    let token =
        serenity::Token::from_env("DISCORD_TOKEN").context("Missing or invalid DISCORD_TOKEN")?;

    let test_guild_id = std::env::var("TEST_GUILD_ID")
        .ok()
        .map(|id| id.parse::<u64>())
        .transpose()
        .context("TEST_GUILD_ID must be a number")?
        .map(serenity::GuildId::new);

    //TODO: Specify the intents that bot needs.
    let intents = serenity::GatewayIntents::all();

    let framework = poise::Framework::new(poise::FrameworkOptions {
        commands: commands::all(),
        ..Default::default()
    });

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(Box::new(framework))
        .event_handler(Arc::new(events::Handler::new(test_guild_id)))
        .data(Arc::new(Data { pool }))
        .await
        .context("Failed to create client")?;

    client.start().await.context("Failed to start client")?;

    info!("Bot started successfully");

    Ok(())
}
