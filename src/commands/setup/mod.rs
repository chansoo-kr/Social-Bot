//! `/setup`: per-server configuration. Each subcommand lives in its own module.

mod language;
mod moderator;
mod status;
mod verification;

pub use language::stored_guild_lang;
pub use status::restore_status;
pub use verification::handle_interaction;

use crate::{Context, Error, commands::verification::moderator_only};

#[poise::command(
    slash_command,
    guild_only,
    check = "moderator_only",
    subcommands(
        "verification::verification",
        "moderator::moderator",
        "language::language",
        "status::status"
    )
)]
pub async fn setup(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}
