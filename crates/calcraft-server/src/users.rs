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
        /// Grant administrator rights (user management in the web UI).
        #[arg(long)]
        admin: bool,
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
    /// Grant or revoke administrator rights.
    SetAdmin {
        username: String,
        /// Revoke instead of grant.
        #[arg(long)]
        revoke: bool,
    },
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
            admin,
            password,
        } => {
            let username = parse_username(&username)?;
            let hash = hash_password(&password.read()?)?;
            store
                .create_user_with_role(&username, &hash, display_name.as_deref(), admin)
                .await?;
            println!(
                "created user {username}{}",
                if admin { " (admin)" } else { "" }
            );
        }
        UserCommand::Passwd { username, password } => {
            let username = parse_username(&username)?;
            let hash = hash_password(&password.read()?)?;
            let Some(record) = store.user_credentials(&username).await? else {
                bail!("user {username} does not exist");
            };
            store.set_password_hash(&username, &hash).await?;
            // Existing web sessions end, e.g. when recovering an account.
            store.delete_sessions_of(record.user.id).await?;
            println!("password of {username} changed");
        }
        UserCommand::List => {
            for user in store.list_users().await? {
                let mut state = String::new();
                if user.is_admin {
                    state.push_str(" (admin)");
                }
                if user.disabled {
                    state.push_str(" (disabled)");
                }
                match &user.display_name {
                    Some(name) => println!("{}\t{name}{state}", user.username),
                    None => println!("{}{state}", user.username),
                }
            }
        }
        UserCommand::Disable { username } => set_disabled(store, &username, true).await?,
        UserCommand::Enable { username } => set_disabled(store, &username, false).await?,
        UserCommand::SetAdmin { username, revoke } => {
            let username = parse_username(&username)?;
            if !store.set_admin(&username, !revoke).await? {
                bail!("user {username} does not exist");
            }
            let state = if revoke { "revoked" } else { "granted" };
            println!("admin rights of {username} {state}");
        }
    }
    Ok(())
}

async fn set_disabled(store: &Store, username: &str, disable: bool) -> anyhow::Result<()> {
    let username = parse_username(username)?;
    let Some(record) = store.user_credentials(&username).await? else {
        bail!("user {username} does not exist");
    };
    store.set_disabled(&username, disable).await?;
    if disable {
        store.delete_sessions_of(record.user.id).await?;
    }
    let state = if disable { "disabled" } else { "enabled" };
    println!("user {username} {state}");
    Ok(())
}
