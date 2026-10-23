//! Passwords read from files (`calcraft migrate --password-file`,
//! `smtp.password_file`): a regular file only its owner can read, wiped
//! from memory when no longer needed.

use std::{io::Read, path::Path};

use anyhow::{Context, bail};
use zeroize::Zeroizing;

/// Largest accepted password file.
const MAX_PASSWORD_FILE: u64 = 4096;

/// A password: wiped from memory when dropped, never printed.
#[derive(Clone)]
pub struct Password(Zeroizing<String>);

impl Password {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Checks that a password file is a regular file (no symlink, FIFO or
/// device), small, and on Unix accessible only by its owner.
fn check_password_file(path: &Path, metadata: &std::fs::Metadata) -> anyhow::Result<()> {
    if !metadata.file_type().is_file() || metadata.len() > MAX_PASSWORD_FILE {
        bail!(
            "password file {} must be a regular file (not a link or device) of at most \
             {MAX_PASSWORD_FILE} bytes",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!(
                "password file {} is accessible by group or others (run chmod 600)",
                path.display()
            );
        }
    }
    Ok(())
}

/// Reads a password from the first line of a file that only its owner can
/// read. The path is checked without following links before it is opened,
/// and the opened file is checked again (and on Unix must be the same file).
pub fn read_password_file(path: &Path) -> anyhow::Result<Password> {
    let before = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to read password file {}", path.display()))?;
    check_password_file(path, &before)?;
    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to open password file {}", path.display()))?;
    let opened = file.metadata()?;
    check_password_file(path, &opened)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (before.dev(), before.ino()) != (opened.dev(), opened.ino()) {
            bail!(
                "password file {} was replaced while reading it",
                path.display()
            );
        }
    }
    // Allocated once with room for the whole file, so no copy of the
    // password is left behind by reallocation.
    let capacity = usize::try_from(MAX_PASSWORD_FILE).unwrap_or(4096) + 1;
    let mut text = Zeroizing::new(String::with_capacity(capacity));
    file.take(MAX_PASSWORD_FILE)
        .read_to_string(&mut text)
        .context("password file is not UTF-8")?;
    let password = text.lines().next().unwrap_or_default();
    if password.is_empty() {
        bail!("password file {} is empty", path.display());
    }
    Ok(Password::new(password))
}
