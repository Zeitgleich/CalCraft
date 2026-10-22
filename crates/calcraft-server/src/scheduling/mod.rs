//! CalDAV scheduling (RFC 6638) between users of this server.
//!
//! Stage A: invitations, cancellations and replies are delivered only to
//! users of this server; any other address gets `SCHEDULE-STATUS` 5.3 (no
//! scheduling), and a later stage can add iMIP at [`Resolution::External`].
//!
//! * **Addresses** ([`Scheduling::addresses`]): the e-mail address an
//!   administrator set (never one taken from an SSO identity provider),
//!   `mailto:<username>@<scheduling.domain>` if a domain is configured, and
//!   the principal URL. Compared case-insensitively ([`normalize`]).
//! * **Who schedules**: only the organizer, i.e. a user whose own address
//!   is the ORGANIZER, sends invitations from their calendars; a PUT with
//!   somebody else as ORGANIZER is stored but schedules nothing, unless the
//!   user is an attendee, and then only their own reply. Spoofing another
//!   organizer therefore reaches nobody.
//! * **Deliveries** ([`deliver`]) go into the recipient's writable
//!   calendars only (never subscriptions), count towards their quota and
//!   are refused with 5.1 instead of failing the sender's request.
//! * **Abuse limits**: attendees per object, recipients per user and hour
//!   ([`Scheduling::allow`]), inbox size; disabled users are treated like
//!   unknown ones.
//!
//! Logs never contain addresses or event data, only user names and counts.

pub mod deliver;
pub mod freebusy;

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use calcraft_core::{CollectionKind, Username};
use calcraft_store::{Collection, Store, StoreError, User};

use crate::{app::DAV_ROOT, config::SchedulingConfig};

/// Window of the per-user recipient limit.
const RATE_WINDOW: Duration = Duration::from_secs(3600);
/// Users tracked by the rate limiter; expired entries are dropped beyond.
const MAX_TRACKED_USERS: usize = 100_000;

/// Runtime state of scheduling (`[scheduling] enabled = true`).
#[derive(Debug)]
pub struct Scheduling {
    pub config: SchedulingConfig,
    /// Per user: start of the current window and recipients used in it.
    usage: Mutex<HashMap<i64, (Instant, u32)>>,
}

/// What a calendar user address refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// An enabled user of this server.
    Local(User),
    /// An address this server is responsible for, but nobody (or a
    /// disabled user) has it: 3.7.
    Unknown,
    /// An address elsewhere: not deliverable in stage A (5.3); iMIP later.
    External,
}

/// Normalised form of a calendar user address, used for every comparison:
/// `mailto:` addresses in lower case (ASCII), principal URLs of this server
/// (absolute or as a path) as `principal:<username>`, anything else in
/// lower case as it is.
pub fn normalize(value: &str) -> String {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if let Some(address) = lower.strip_prefix("mailto:") {
        return format!("mailto:{address}");
    }
    let path = if lower.starts_with("http://") || lower.starts_with("https://") {
        value
            .split_once("://")
            .and_then(|(_, rest)| rest.find('/').map(|i| &rest[i..]))
    } else if value.starts_with('/') {
        Some(value)
    } else {
        None
    };
    if let Some(path) = path
        && let Some(user) = path
            .strip_prefix(DAV_ROOT)
            .and_then(|rest| rest.strip_prefix("principals/"))
            .map(|rest| rest.strip_suffix('/').unwrap_or(rest))
        && let Ok(user) = user.parse::<Username>()
    {
        return format!("principal:{user}");
    }
    lower
}

/// Validates and normalises an e-mail address for a user (set by an
/// administrator): ASCII only, `local@domain`, at most 254 characters,
/// returned in lower case without `mailto:`.
pub fn normalize_email(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value
        .strip_prefix("mailto:")
        .or_else(|| value.strip_prefix("MAILTO:"))
        .unwrap_or(value);
    let lower = value.to_ascii_lowercase();
    let (local, domain) = lower.split_once('@')?;
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~.".contains(&b));
    let domain_ok = domain.len() <= 253
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    (local_ok && domain_ok && lower.len() <= calcraft_store::MAX_EMAIL_LEN).then_some(lower)
}

/// A random opaque value for a Schedule-Tag (quoted like an entity tag).
pub fn new_schedule_tag() -> String {
    format!("\"{}\"", crate::secrets::random_token::<12>())
}

/// A random resource name for a delivered object or inbox message.
pub fn new_resource_name() -> String {
    format!("{}.ics", crate::secrets::random_token::<16>())
}

/// The current UTC time as an iCalendar DATE-TIME.
pub fn dtstamp() -> String {
    calcraft_ical::itip::utc_stamp(crate::auth::now()).unwrap_or_else(|| "19700101T000000Z".into())
}

impl Scheduling {
    pub fn new(config: &SchedulingConfig) -> Self {
        Self {
            config: config.clone(),
            usage: Mutex::new(HashMap::new()),
        }
    }

    /// The address `mailto:<username>@<domain>`, if a domain is configured.
    pub fn derived_address(&self, username: &Username) -> Option<String> {
        self.config
            .domain
            .as_ref()
            .map(|domain| format!("mailto:{username}@{domain}"))
    }

    /// The user's calendar user addresses as shown to clients
    /// (`calendar-user-address-set`), preferred first.
    pub async fn address_set(&self, store: &Store, user: &User) -> Result<Vec<String>, StoreError> {
        let mut set = Vec::new();
        if let Some(email) = store.user_email(user.id).await? {
            set.push(format!("mailto:{email}"));
        }
        if let Some(derived) = self.derived_address(&user.username)
            && !set.contains(&derived)
        {
            set.push(derived);
        }
        set.push(crate::dav::principal_href(&user.username));
        Ok(set)
    }

    /// The user's addresses, normalised.
    pub async fn addresses(&self, store: &Store, user: &User) -> Result<Vec<String>, StoreError> {
        Ok(self
            .address_set(store, user)
            .await?
            .iter()
            .map(|a| normalize(a))
            .collect())
    }

    /// Resolves a normalised address. An address that is both one user's
    /// e-mail address and another user's derived address is refused as
    /// unknown (administrators are kept from creating that case, see
    /// [`Scheduling::email_conflicts`]).
    pub async fn resolve(&self, store: &Store, address: &str) -> Result<Resolution, StoreError> {
        let enabled = |user: Option<User>| match user {
            Some(user) if !user.disabled => Resolution::Local(user),
            _ => Resolution::Unknown,
        };
        if let Some(name) = address.strip_prefix("principal:") {
            let Ok(name) = name.parse::<Username>() else {
                return Ok(Resolution::Unknown);
            };
            return Ok(enabled(store.user_by_name(&name).await?));
        }
        let Some(email) = address.strip_prefix("mailto:") else {
            return Ok(Resolution::External);
        };
        let explicit = store.user_by_email(email).await?;
        let derived = match (&self.config.domain, email.rsplit_once('@')) {
            (Some(domain), Some((local, host))) if host == domain => {
                match local.parse::<Username>() {
                    Ok(name) => store.user_by_name(&name).await?,
                    Err(_) => None,
                }
            }
            _ => None,
        };
        let ours = self.config.domain.as_deref().is_some_and(|domain| {
            email
                .rsplit_once('@')
                .is_some_and(|(_, host)| host == domain)
        });
        Ok(match (explicit, derived) {
            (Some(a), Some(b)) if a.id != b.id => {
                tracing::warn!(
                    user = %a.username,
                    other = %b.username,
                    "calendar user address is ambiguous; not delivering"
                );
                Resolution::Unknown
            }
            (Some(user), _) | (None, Some(user)) => enabled(Some(user)),
            (None, None) if ours => Resolution::Unknown,
            (None, None) => Resolution::External,
        })
    }

    /// Takes up to `wanted` recipients from the user's hourly budget and
    /// returns how many were granted.
    pub fn allow(&self, user_id: i64, wanted: u32) -> u32 {
        let limit = self.config.max_recipients_per_hour;
        let now = Instant::now();
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if usage.len() >= MAX_TRACKED_USERS && !usage.contains_key(&user_id) {
            usage.retain(|_, (start, _)| now.duration_since(*start) < RATE_WINDOW);
            if usage.len() >= MAX_TRACKED_USERS {
                return 0;
            }
        }
        let entry = usage.entry(user_id).or_insert((now, 0));
        if now.duration_since(entry.0) >= RATE_WINDOW {
            *entry = (now, 0);
        }
        let granted = wanted.min(limit.saturating_sub(entry.1));
        entry.1 += granted;
        if granted < wanted {
            tracing::warn!(user_id, wanted, granted, "scheduling rate limit reached");
        }
        granted
    }
}

/// Why an e-mail address cannot be assigned to a user.
#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    #[error("invalid e-mail address (ASCII only, local@domain)")]
    Invalid,
    #[error("the address is already used by another user")]
    Taken,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Sets or removes (`None` or blank) a user's e-mail address, as an
/// administrator does: validated ([`normalize_email`]), unique, and never
/// another user's derived address `<username>@<domain>`.
pub async fn set_email(
    store: &Store,
    domain: Option<&str>,
    user: &User,
    raw: Option<&str>,
) -> Result<Option<String>, EmailError> {
    let raw = raw.map(str::trim).filter(|r| !r.is_empty());
    let Some(raw) = raw else {
        store.set_user_email(user.id, None).await?;
        return Ok(None);
    };
    let email = normalize_email(raw).ok_or(EmailError::Invalid)?;
    // Addresses in the scheduling domain are derived from user names: only
    // a user's own one may be set (it changes nothing then).
    if let (Some(domain), Some((local, host))) = (domain, email.rsplit_once('@'))
        && host == domain
        && local != user.username.as_str()
    {
        return Err(EmailError::Taken);
    }
    match store.set_user_email(user.id, Some(&email)).await {
        Ok(_) => Ok(Some(email)),
        Err(StoreError::AddressTaken) => Err(EmailError::Taken),
        Err(err) => Err(err.into()),
    }
}

/// Whether a new user's derived address `<username>@<domain>` is already
/// another user's e-mail address.
pub async fn derived_address_taken(
    store: &Store,
    domain: Option<&str>,
    username: &Username,
) -> Result<bool, StoreError> {
    let Some(domain) = domain else {
        return Ok(false);
    };
    Ok(store
        .user_by_email(&format!("{username}@{domain}"))
        .await?
        .is_some())
}

/// The calendar a new invitation goes to (`schedule-default-calendar-URL`):
/// the writable calendar `default` if it takes `component`, else the first
/// writable one that does. Subscriptions never.
pub async fn default_calendar(
    store: &Store,
    owner_id: i64,
    component: &str,
) -> Result<Option<Collection>, StoreError> {
    let calendars = store
        .collections(owner_id, CollectionKind::Calendar)
        .await?;
    let fits = |c: &&Collection| !c.read_only && c.components.split(',').any(|k| k == component);
    Ok(calendars
        .iter()
        .find(|c| c.slug.as_str() == calcraft_store::DEFAULT_COLLECTION_SLUG && fits(c))
        .or_else(|| calendars.iter().find(fits))
        .cloned())
}

#[cfg(test)]
mod tests;
