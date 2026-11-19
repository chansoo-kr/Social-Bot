use std::sync::atomic::{AtomicBool, Ordering};

use crate::{Data, Error, commands};
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
            _ => {}
        }

        Ok(())
    }

    async fn register_commands(&self, ctx: &serenity::Context) -> Result<(), Error> {
        let commands = commands::all();
        match self.test_guild_id {
            Some(guild_id) => {
                poise::builtins::register_in_guild(&ctx.http, &commands, guild_id).await?;
                info!("Registered commands in test guild {guild_id}");
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
