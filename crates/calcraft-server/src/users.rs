//! `calcraft user` subcommands.

use std::io::BufRead;

use anyhow::{Context, bail};
use calcraft_core::Username;
use calcraft_server::auth::hash_password;
use calcraft_store::Store;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum UserCommand {
    /// Create a user with a default calendar and address book.
    Add {
        username: String,
        /// Name shown in clients.
        #[arg(long)]
        display_name: Option<String>,
        #[command(flatten)]
        password: PasswordInput,
    },
    /// Set a new password.
    Passwd {
        username: String,
        #[command(flatten)]
        password: PasswordInput,
    },
    /// List all users.
    List,
    /// Prevent a user from logging in.
    Disable { username: String },
    /// Allow a disabled user to log in again.
    Enable { username: String },
}

/// Passwords are never accepted as command-line arguments, because those
/// end up in shell history and process listings.
#[derive(Debug, Args)]
pub struct PasswordInput {
    /// Read the password from the first line of standard input.
    #[arg(long)]
    password_stdin: bool,
}

impl PasswordInput {
    fn read(&self) -> anyhow::Result<String> {
        if self.password_stdin {
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .context("failed to read password from stdin")?;
            return Ok(line.trim_end_matches(['\r', '\n']).to_owned());
        }
        let password = rpassword::prompt_password("Password: ")?;
        let repeated = rpassword::prompt_password("Repeat password: ")?;
        if password != repeated {
            bail!("passwords do not match");
        }
        Ok(password)
    }
}

fn parse_username(name: &str) -> anyhow::Result<Username> {
    Ok(name.parse()?)
}

pub async fn run(store: &Store, command: UserCommand) -> anyhow::Result<()> {
    match command {
        UserCommand::Add {
            username,
            display_name,
            password,
        } => {
            let username = parse_username(&username)?;
            let hash = hash_password(&password.read()?)?;
            store
                .create_user(&username, &hash, display_name.as_deref())
                .await?;
            println!("created user {username}");
        }
        UserCommand::Passwd { username, password } => {
            let username = parse_username(&username)?;
            let hash = hash_password(&password.read()?)?;
            if !store.set_password_hash(&username, &hash).await? {
                bail!("user {username} does not exist");
            }
            println!("password of {username} changed");
        }
        UserCommand::List => {
            for user in store.list_users().await? {
                let state = if user.disabled { " (disabled)" } else { "" };
                match &user.display_name {
                    Some(name) => println!("{}\t{name}{state}", user.username),
                    None => println!("{}{state}", user.username),
                }
            }
        }
        UserCommand::Disable { username } => set_disabled(store, &username, true).await?,
        UserCommand::Enable { username } => set_disabled(store, &username, false).await?,
    }
    Ok(())
}

async fn set_disabled(store: &Store, username: &str, disable: bool) -> anyhow::Result<()> {
    let username = parse_username(username)?;
    if !store.set_disabled(&username, disable).await? {
        bail!("user {username} does not exist");
    }
    let state = if disable { "disabled" } else { "enabled" };
    println!("user {username} {state}");
    Ok(())
}
