//! `calcraft smtp` subcommands.

use anyhow::{Context, bail};
use calcraft_core::Username;
use calcraft_server::{Config, reminders::Reminders};
use calcraft_store::Store;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum SmtpCommand {
    /// Send one test message with the `[smtp]` settings to a user's own
    /// address (the one reminders go to), right away. The address is not
    /// printed.
    Test {
        /// The user whose own address receives the message.
        #[arg(long = "to-self", value_name = "USER")]
        to_self: String,
    },
}

pub async fn run(store: &Store, config: &Config, command: SmtpCommand) -> anyhow::Result<()> {
    match command {
        SmtpCommand::Test { to_self } => {
            let name: Username = to_self
                .trim()
                .to_ascii_lowercase()
                .parse()
                .context("invalid user name")?;
            let Some(user) = store.user_by_name(&name).await? else {
                bail!("no such user");
            };
            let reminders =
                Reminders::new(store.clone(), config).context("invalid [smtp] configuration")?;
            reminders
                .send_test_now(&user)
                .await
                .context("sending the test message failed")?;
            println!("test message sent");
            Ok(())
        }
    }
}
