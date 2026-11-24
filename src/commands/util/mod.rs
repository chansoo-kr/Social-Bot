use crate::{Context, Error, commands::verification::moderator_only};
use poise::serenity_prelude as serenity;

mod embed;
mod placeholder;

#[poise::command(
    slash_command,
    guild_only,
    check = "moderator_only",
    subcommands("embed::embed", "placeholder::placeholder")
)]
pub async fn util(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Handles component and modal interactions whose custom ID starts with `embed:`.
pub async fn handle_interaction(
    ctx: &serenity::Context,
    interaction: &serenity::Interaction,
) -> Result<(), Error> {
    embed::handle_interaction(ctx, interaction).await
}
