use crate::{Context, Error, commands::verification::moderator_only};

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
