//! `calcraft group` subcommands. Group names and usernames are never
//! repeated in the output of a change (the caller just typed them).

use anyhow::bail;
use calcraft_core::{GroupName, Username};
use calcraft_server::{Config, auth::now, validate};
use calcraft_store::{Group, Store, StoreError, User};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum GroupCommand {
    /// Create a group (name rules like usernames).
    Add {
        name: String,
        /// Name shown in the web UI.
        #[arg(long)]
        display_name: Option<String>,
    },
    /// Delete a group; all its shares end at once.
    Remove { name: String },
    /// Add a user to a group: they are invited to every collection shared
    /// with the group.
    AddMember { group: String, username: String },
    /// Remove a user from a group: the group's shares end for them at once.
    RemoveMember { group: String, username: String },
    /// List all groups, or the members of one.
    List {
        /// List the members of this group.
        group: Option<String>,
    },
}

const INVALID_GROUP: &str = "invalid group name: use 1-64 characters a-z, 0-9, '.', '_' or '-', starting with a letter or digit";

async fn group(store: &Store, name: &str) -> anyhow::Result<Group> {
    let name: GroupName = name.parse().map_err(|_| anyhow::anyhow!(INVALID_GROUP))?;
    match store.group_by_name(&name).await? {
        Some(group) => Ok(group),
        None => bail!("no such group"),
    }
}

async fn user(store: &Store, name: &str) -> anyhow::Result<User> {
    let name: Username = name
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid username"))?;
    match store.user_by_name(&name).await? {
        Some(user) => Ok(user),
        None => bail!("no such user"),
    }
}

pub async fn run(store: &Store, config: &Config, command: GroupCommand) -> anyhow::Result<()> {
    match command {
        GroupCommand::Add { name, display_name } => {
            let name: GroupName = name.parse().map_err(|_| anyhow::anyhow!(INVALID_GROUP))?;
            let display_name = match display_name.as_deref() {
                Some(text) => match validate::clean_text(text, validate::MAX_DISPLAY_NAME) {
                    Some(text) => Some(text).filter(|t| !t.is_empty()),
                    None => bail!("invalid display name"),
                },
                None => None,
            };
            match store
                .create_group(
                    &name,
                    display_name.as_deref(),
                    config.groups.max_groups,
                    now(),
                )
                .await
            {
                Ok(_) => println!("group created"),
                Err(StoreError::GroupExists) => bail!("the group exists already"),
                Err(StoreError::Limit(_)) => {
                    bail!("too many groups (groups.max_groups)")
                }
                Err(err) => return Err(err.into()),
            }
        }
        GroupCommand::Remove { name } => {
            let group = group(store, &name).await?;
            store.delete_group(group.id).await?;
            println!("group deleted");
        }
        GroupCommand::AddMember {
            group: name,
            username,
        } => {
            let group = group(store, &name).await?;
            let user = user(store, &username).await?;
            match store
                .add_group_member(group.id, user.id, config.groups.max_members, now())
                .await
            {
                Ok(added) if added.added => {
                    println!("member added, {} new invitations", added.invited);
                }
                Ok(_) => println!("already a member"),
                Err(StoreError::Limit(_)) => {
                    bail!("the group is full (groups.max_members)")
                }
                Err(err) => return Err(err.into()),
            }
        }
        GroupCommand::RemoveMember {
            group: name,
            username,
        } => {
            let group = group(store, &name).await?;
            let user = user(store, &username).await?;
            if !store.remove_group_member(group.id, user.id).await? {
                bail!("not a member");
            }
            println!("member removed");
        }
        GroupCommand::List { group: None } => {
            for group in store.list_groups().await? {
                let name = group.display_name.unwrap_or_default();
                println!("{}\t{}\t{name}", group.name, group.members);
            }
        }
        GroupCommand::List { group: Some(name) } => {
            let group = group(store, &name).await?;
            for member in store.group_members(group.id).await? {
                let state = if member.disabled { " (disabled)" } else { "" };
                println!("{}{state}", member.username);
            }
        }
    }
    Ok(())
}
