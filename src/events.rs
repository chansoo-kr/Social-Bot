use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    Data, Error,
    commands::{self},
    i18n,
};
use poise::serenity_prelude as serenity;
use tracing::{error, info};

pub struct Handler {
    test_guild_id: Option<serenity::GuildId>,
    commands_registered: AtomicBool,
}

impl Handler {
    pub fn new(test_guild_id: Option<serenity::GuildId>) -> Self {
        Self {
            test_guild_id,
            commands_registered: AtomicBool::new(false),
        }
    }

    async fn handle(
        &self,
        ctx: &serenity::Context,
        event: &serenity::FullEvent,
    ) -> Result<(), Error> {
        let pool = &ctx.data_ref::<Data>().pool;

        match event {
            serenity::FullEvent::Ready { data_about_bot, .. } => {
                info!("{} is connected!", data_about_bot.user.name);

                if !self.commands_registered.swap(true, Ordering::SeqCst) {
                    self.register_commands(ctx).await?;
                }

                let current: Vec<i64> = data_about_bot
                    .guilds
                    .iter()
                    .map(|g| g.id.get() as i64)
                    .collect();
                sqlx::query(
                    "UPDATE guilds SET is_active = false, left_at = now()
                    WHERE is_active AND guild_id <> ALL($1)",
                )
                .bind(&current)
                .execute(pool)
                .await?;

                commands::setup::restore_status(ctx).await;
            }

            serenity::FullEvent::GuildCreate {
                guild: serenity::GuildCreateGuild::Available(guild),
                is_new,
                ..
            } => {
                sqlx::query(
                    "INSERT INTO guilds (guild_id, name) VALUES ($1, $2)
                     ON CONFLICT (guild_id) DO UPDATE
                     SET name = EXCLUDED.name, is_active = true, left_at = NULL",
                )
                .bind(guild.id.get() as i64)
                .bind(guild.name.as_str())
                .execute(pool)
                .await?;

                if *is_new == Some(true) {
                    info!("Joined guild {} ({})", guild.name, guild.id);
                }
            }

            serenity::FullEvent::GuildDelete { incomplete, .. } if !incomplete.unavailable => {
                sqlx::query(
                    "UPDATE guilds SET is_active = false, left_at = now() WHERE guild_id = $1",
                )
                .bind(incomplete.id.get() as i64)
                .execute(pool)
                .await?;

                info!("Left guild {}", incomplete.id);
            }

            serenity::FullEvent::InteractionCreate { interaction, .. } => {
                // Slash commands are dispatched by poise; they only pass through here so the
                // user's locale gets recorded.
                let (user, locale, custom_id) = match interaction {
                    serenity::Interaction::Command(command) => {
                        (&command.user, &command.locale, None)
                    }
                    serenity::Interaction::Component(component) => (
                        &component.user,
                        &component.locale,
                        Some(component.data.custom_id.as_str()),
                    ),
                    serenity::Interaction::Modal(modal) => (
                        &modal.user,
                        &modal.locale,
                        Some(modal.data.custom_id.as_str()),
                    ),
                    _ => return Ok(()),
                };
                i18n::remember(pool, user.id, locale);

                match custom_id.and_then(|id| id.split(':').next()) {
                    Some("verify") => {
                        commands::setup::handle_interaction(ctx, interaction).await?;
                        commands::verification::handle_interaction(ctx, interaction).await?;
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        Ok(())
    }

    async fn register_commands(&self, ctx: &serenity::Context) -> Result<(), Error> {
        let commands = commands::all();
        match self.test_guild_id {
            Some(guild_id) => {
                poise::builtins::register_in_guild(&ctx.http, &commands, guild_id).await?;
                // Guild and global commands are separate lists, so anything registered globally
                // by an earlier run without TEST_GUILD_ID would linger next to these as a stale
                // duplicate. This also wipes the global commands of any other deployment that
                // shares this bot's token.
                serenity::Command::set_global_commands(&ctx.http, &[]).await?;
                info!("Registered commands in test guild {guild_id} and cleared global commands");
            }
            None => {
                poise::builtins::register_globally(&ctx.http, &commands).await?;
                info!("Registered commands globally");
            }
        }
        Ok(())
    }
}

#[serenity::async_trait]
impl serenity::EventHandler for Handler {
    async fn dispatch(&self, ctx: &serenity::Context, event: &serenity::FullEvent) {
        if let Err(e) = self.handle(ctx, event).await {
            error!("Error while handling event: {e}");
        }
    }
}
