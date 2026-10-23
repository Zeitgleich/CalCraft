//! Groups of users (docs/sharing.md, "Groups"): who may share with a group.
//!
//! Administrators manage groups (CLI `calcraft group`, REST API, web UI).
//! A user may share a collection with a group they are a member of;
//! administrators with every group. For everybody else a group does not
//! exist: sharing with it answers like an unknown group, so its name
//! cannot be probed. Group names are never logged.

use calcraft_core::GroupName;
use calcraft_store::{Group, Store, StoreError, User};

/// Group limits from `[groups]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupLimits {
    /// Most groups on the server.
    pub max_groups: u32,
    /// Most members of one group.
    pub max_members: u32,
}

impl Default for GroupLimits {
    fn default() -> Self {
        let config = crate::config::GroupsConfig::default();
        Self {
            max_groups: config.max_groups,
            max_members: config.max_members,
        }
    }
}

impl From<&crate::config::GroupsConfig> for GroupLimits {
    fn from(config: &crate::config::GroupsConfig) -> Self {
        Self {
            max_groups: config.max_groups,
            max_members: config.max_members,
        }
    }
}

/// The group `name` if `user` may share with it (member or administrator),
/// else `None`, also for unknown names.
pub async fn shareable_group(
    store: &Store,
    user: &User,
    name: &str,
) -> Result<Option<Group>, StoreError> {
    let Ok(name) = name.parse::<GroupName>() else {
        return Ok(None);
    };
    let Some(group) = store.group_by_name(&name).await? else {
        return Ok(None);
    };
    if user.is_admin || store.is_group_member(group.id, user.id).await? {
        Ok(Some(group))
    } else {
        Ok(None)
    }
}
