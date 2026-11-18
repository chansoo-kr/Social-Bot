use anyhow::{Context as _, Ok};
use poise::serenity_prelude as serenity;

struct Data {}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

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
                println!("{} is connected!", ready.user.name);

                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(Data {})
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .context("Failed to create client")?;

    client.start().await.context("Failed to start client")?;

    Ok(())
}
