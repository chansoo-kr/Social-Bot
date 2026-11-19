use anyhow::{Context as _, Ok};
use poise::serenity_prelude as serenity;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

struct Data {
    pool: sqlx::Pool<sqlx::Postgres>,
}

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

    //TODO: Specify the intents that bot needs.
    let intents = serenity::GatewayIntents::all();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![],
            ..Default::default()
        })
        .setup(|ctx, ready, framework| {
            Box::pin(async move {
                info!("{} is connected!", ready.user.name);

                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
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
