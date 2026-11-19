use anyhow::Context as _;
use poise::serenity_prelude::{self as serenity};
use sqlx::postgres::PgPoolOptions;
use tracing::info;

mod commands;

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

    let token = std::env::var("DISCORD_TOKEN").context("Missing DISCORD_TOKEN")?;

    let test_guild_id = std::env::var("TEST_GUILD_ID")
        .ok()
        .map(|id| id.parse::<u64>())
        .transpose()
        .context("TEST_GUILD_ID must be a number")?
        .map(serenity::GuildId::new);

    //TODO: Specify the intents that bot needs.
    let intents = serenity::GatewayIntents::all();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![commands::setup::setup()],
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                info!("{} is connected!", ready.user.name);

                let commands = &framework.options().commands;
                match test_guild_id {
                    Some(guild_id) => {
                        poise::builtins::register_in_guild(ctx, commands, guild_id).await?;
                        info!("Registered commands in test guild {guild_id}");
                    }
                    None => {
                        poise::builtins::register_globally(ctx, commands).await?;
                        info!("Registered commands globally");
                    }
                }

                Ok(Data { pool })
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .context("Failed to create client")?;

    client.start().await.context("Failed to start client")?;

    info!("Bot started successfully");

    Ok(())
}
