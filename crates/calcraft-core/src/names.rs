//! Validated identifiers that appear in URLs.
//!
//! Usernames and collection slugs are part of DAV paths. Restricting them to
//! a small ASCII alphabet rules out path traversal, percent-encoding
//! ambiguities and Unicode confusables by construction.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// Error for a name that does not satisfy the naming rules.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid {kind} {value:?}: use 1-{max} characters a-z, 0-9, '.', '_' or '-', starting with a letter or digit"
)]
pub struct InvalidName {
    kind: &'static str,
    value: String,
    max: usize,
}

fn validate(kind: &'static str, max: usize, value: &str) -> Result<(), InvalidName> {
    let valid = !value.is_empty()
        && value.len() <= max
        && value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if valid {
        Ok(())
    } else {
        Err(InvalidName {
            kind,
            value: value.chars().take(max + 1).collect(),
            max,
        })
    }
}

macro_rules! name_type {
    ($(#[$doc:meta])* $name:ident, $kind:literal, $max:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Maximum length in bytes.
            pub const MAX_LEN: usize = $max;

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = InvalidName;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                validate($kind, $max, value)?;
                Ok(Self(value.to_owned()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidName;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                validate($kind, $max, &value)?;
                Ok(Self(value))
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(
    /// Login name of a user. Lowercase so that logins are case-insensitive.
    Username,
    "username",
    64
);

name_type!(
    /// Name of a group of users (same rules as usernames). Appears in the
    /// group principal URL `/dav/principals/groups/<name>/`.
    GroupName,
    "group name",
    64
);

name_type!(
    /// URL segment of a calendar or address book.
    Slug,
    "collection name",
    64
);

/// Type of a DAV collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionKind {
    Calendar,
    Addressbook,
}

impl CollectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calendar => "calendar",
            Self::Addressbook => "addressbook",
        }
    }
}

impl FromStr for CollectionKind {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "calendar" => Ok(Self::Calendar),
            "addressbook" => Ok(Self::Addressbook),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests;
