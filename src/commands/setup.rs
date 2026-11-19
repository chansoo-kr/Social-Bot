use crate::{Context, Error};
use poise::serenity_prelude::{self as serenity, Mentionable};

#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    subcommands("verification")
)]
pub async fn setup(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, guild_only)]
async fn verification(
    ctx: Context<'_>,
    #[description = "Verification channel"] channel: serenity::GuildChannel,
    #[description = "Role to assign after verification"] role: serenity::Role,
) -> Result<(), Error> {
    let embed = serenity::CreateEmbed::new()
        .title("✅ Verification setup complete")
        .color(0x57F287)
        .field("Channel", channel.mention().to_string(), true)
        .field("Role", role.mention().to_string(), true)
        .footer(serenity::CreateEmbedFooter::new(format!(
            "Configured by {}",
            ctx.author().name
        )))
        .timestamp(serenity::Timestamp::now());

    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}
