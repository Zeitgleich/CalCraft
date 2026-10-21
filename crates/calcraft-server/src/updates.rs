//! Optional update notice for administrators (`[updates] check`, off by
//! default).
//!
//! A background task asks GitHub for the latest release at startup and then
//! every `interval_hours` (plus up to 10 % random jitter). The request is a
//! plain `GET` of the fixed [`RELEASES_URL`] with `User-Agent:
//! calcraft/<version>`: no cookies, no credentials, no data about the
//! installation. GitHub sees the server's IP address and that User-Agent.
//!
//! Only `tag_name`, `html_url`, `prerelease` and `draft` are read; drafts,
//! pre-releases and tags that are not `vX.Y.Z` are ignored, and the link is
//! only kept if it points to this repository's release pages on github.com.
//! The result lives in memory. Failures are logged (without response
//! bodies) and never affect serving.

use std::{
    fmt,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use axum::http::{StatusCode, Uri};
use calcraft_core::api::UpdateInfo;
use serde::Deserialize;

use crate::{
    auth::now,
    config::UpdatesConfig,
    https::{HttpClient, HttpError, MAX_RESPONSE},
    secrets::random_bytes,
};

/// The only URL the update check requests.
pub const RELEASES_URL: &str = "https://api.github.com/repos/Zeitgleich/CalCraft/releases/latest";
/// Release links shown in the admin area must start with this.
pub const RELEASE_PAGE_PREFIX: &str = "https://github.com/Zeitgleich/CalCraft/releases/";
const MAX_TAG_LEN: usize = 64;
const MAX_URL_LEN: usize = 512;

/// A release version `MAJOR.MINOR.PATCH`, ordered numerically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parses `X.Y.Z` (decimal, no leading zeros, nothing else).
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.').map(|part| {
            let leading_zero = part.len() > 1 && part.starts_with('0');
            if part.is_empty() || leading_zero || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        });
        let version = Self {
            major: parts.next()??,
            minor: parts.next()??,
            patch: parts.next()??,
        };
        parts.next().is_none().then_some(version)
    }

    /// Parses a release tag `vX.Y.Z`.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::parse(tag.strip_prefix('v')?)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Whether `latest` is newer than the running version `current`
/// (`X.Y.Z`, optionally with a `-pre`/`+build` suffix, which counts as
/// older than the release `X.Y.Z`). An unparsable `current` never asks for
/// an update.
pub fn is_newer(latest: Version, current: &str) -> bool {
    let (core, suffix) = match current.find(['-', '+']) {
        Some(at) => (&current[..at], current[at..].starts_with('-')),
        None => (current, false),
    };
    match Version::parse(core) {
        Some(current) => latest > current || (latest == current && suffix),
        None => false,
    }
}

/// Keeps a release link only if it is a plain URL below
/// [`RELEASE_PAGE_PREFIX`] (it is shown as a link to administrators).
pub fn release_link(url: &str) -> Option<&str> {
    let rest = url.strip_prefix(RELEASE_PAGE_PREFIX)?;
    let plain = rest
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/'));
    (url.len() <= MAX_URL_LEN && plain && !rest.contains("..")).then_some(url)
}

/// A stable release from the releases API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// Release page, if it passed [`release_link`].
    pub url: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReleaseError {
    #[error("release information too large")]
    TooLarge,
    /// Only the position is kept: serde messages can quote the input.
    #[error("malformed release information (line {line}, column {column})")]
    Malformed { line: usize, column: usize },
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: Option<String>,
    html_url: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

/// Parses a response of the "latest release" API. `Ok(None)` for drafts,
/// pre-releases and tags that are not `vX.Y.Z`.
pub fn parse_release(body: &[u8]) -> Result<Option<Release>, ReleaseError> {
    if body.len() > MAX_RESPONSE {
        return Err(ReleaseError::TooLarge);
    }
    let raw: RawRelease = serde_json::from_slice(body).map_err(|err| ReleaseError::Malformed {
        line: err.line(),
        column: err.column(),
    })?;
    if raw.draft || raw.prerelease {
        return Ok(None);
    }
    let Some(version) = raw
        .tag_name
        .as_deref()
        .filter(|tag| tag.len() <= MAX_TAG_LEN)
        .and_then(Version::from_tag)
    else {
        return Ok(None);
    };
    let url = raw
        .html_url
        .as_deref()
        .and_then(release_link)
        .map(str::to_owned);
    Ok(Some(Release { version, url }))
}

#[derive(Debug)]
struct Checked {
    release: Option<Release>,
    at: i64,
}

/// In-memory result of the update check.
#[derive(Debug, Default)]
pub struct UpdateCheck {
    last: Mutex<Option<Checked>>,
}

impl UpdateCheck {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a response body of the releases API received at `now`. On
    /// error the previous result is kept.
    pub fn apply(&self, body: &[u8], now: i64) -> Result<(), ReleaseError> {
        let release = parse_release(body)?;
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Checked { release, at: now });
        Ok(())
    }

    /// What `GET /api/v1/admin/update` reports while the check is enabled.
    pub fn info(&self) -> UpdateInfo {
        let current = calcraft_core::VERSION;
        let last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let release = last.as_ref().and_then(|c| c.release.as_ref());
        UpdateInfo {
            enabled: true,
            current: current.to_owned(),
            latest: release.map(|r| r.version.to_string()),
            newer: release.is_some_and(|r| is_newer(r.version, current)),
            url: release.and_then(|r| r.url.clone()),
            checked_at: last.as_ref().map(|c| c.at),
        }
    }
}

/// `interval` plus a random delay of up to a tenth of it.
fn with_jitter(interval: Duration) -> Duration {
    let max = interval.as_secs() / 10;
    let jitter = u64::from_le_bytes(random_bytes()) % (max + 1);
    interval + Duration::from_secs(jitter)
}

/// Starts the background check. Errors (no trust store) are logged; the
/// check then stays without result.
pub fn spawn(config: &UpdatesConfig) -> Arc<UpdateCheck> {
    let check = Arc::new(UpdateCheck::new());
    let client = match HttpClient::new(None, false) {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(%err, "update check disabled: cannot create HTTPS client");
            return check;
        }
    };
    let uri = Uri::from_static(RELEASES_URL);
    let interval = config.interval();
    tokio::spawn({
        let check = check.clone();
        async move {
            loop {
                match client.get(&uri).await {
                    Ok(body) => match check.apply(&body, now()) {
                        Ok(()) => tracing::debug!(info = ?check.info(), "update check done"),
                        Err(err) => tracing::warn!(%err, "update check failed"),
                    },
                    // GitHub answers 404 while the repository has no published release.
                    Err(HttpError::Status(StatusCode::NOT_FOUND)) => {
                        tracing::info!("update check: no published release yet");
                    }
                    // HttpError never contains the response body.
                    Err(err) => tracing::warn!(%err, "update check failed"),
                }
                tokio::time::sleep(with_jitter(interval)).await;
            }
        }
    });
    check
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn versions_parse_strictly() {
        assert_eq!(Version::from_tag("v1.2.3"), Some(v(1, 2, 3)));
        assert_eq!(Version::from_tag("v0.10.0"), Some(v(0, 10, 0)));
        assert_eq!(
            Version::from_tag("v18446744073709551615.0.0"),
            Some(v(u64::MAX, 0, 0))
        );
        for bad in [
            "1.2.3",
            "v1.2",
            "v1.2.3.4",
            "v1.2.3-rc1",
            "v1.2.3+build",
            "v01.2.3",
            "v1..3",
            "v1.2.",
            "v+1.2.3",
            "v-1.2.3",
            "v 1.2.3",
            "v1.2.3 ",
            "V1.2.3",
            "v18446744073709551616.0.0",
            "v١.2.3",
            "",
            "v",
        ] {
            assert_eq!(Version::from_tag(bad), None, "{bad}");
        }
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(v(0, 10, 0) > v(0, 9, 9));
        assert!(v(1, 0, 0) > v(0, 99, 99));
        assert!(v(1, 2, 10) > v(1, 2, 9));
        assert!(is_newer(v(0, 2, 0), "0.1.0"));
        assert!(is_newer(v(0, 1, 1), "0.1.0"));
        assert!(!is_newer(v(0, 1, 0), "0.1.0"));
        assert!(!is_newer(v(0, 0, 9), "0.1.0"));
        // A pre-release is older than its release, build metadata is not.
        assert!(is_newer(v(1, 0, 0), "1.0.0-rc.1"));
        assert!(!is_newer(v(1, 0, 0), "1.0.0+local"));
        assert!(!is_newer(v(0, 9, 0), "1.0.0-rc.1"));
        assert!(!is_newer(v(9, 9, 9), "garbage"));
    }

    #[test]
    fn only_release_pages_of_this_repository_are_links() {
        for ok in [
            "https://github.com/Zeitgleich/CalCraft/releases/tag/v1.2.3",
            "https://github.com/Zeitgleich/CalCraft/releases/latest",
        ] {
            assert_eq!(release_link(ok), Some(ok));
        }
        for bad in [
            "http://github.com/Zeitgleich/CalCraft/releases/tag/v1.2.3",
            "https://github.com/Zeitgleich/CalCraft/releasesx",
            "https://github.com/Zeitgleich/CalCraft/releases",
            "https://github.com/Other/CalCraft/releases/tag/v1.2.3",
            "https://github.com.evil.example/Zeitgleich/CalCraft/releases/x",
            "https://github.com/Zeitgleich/CalCraft/releases/../../../evil",
            "https://github.com/Zeitgleich/CalCraft/releases/tag/v1?x=1",
            "https://github.com/Zeitgleich/CalCraft/releases/tag/v1#x",
            "https://github.com/Zeitgleich/CalCraft/releases/tag/\"onclick",
            "https://github.com/Zeitgleich/CalCraft/releases/tag/v1 x",
            "javascript:alert(1)//https://github.com/Zeitgleich/CalCraft/releases/",
            "",
        ] {
            assert_eq!(release_link(bad), None, "{bad}");
        }
        let long = format!("{RELEASE_PAGE_PREFIX}{}", "a".repeat(MAX_URL_LEN));
        assert_eq!(release_link(&long), None);
    }

    fn release_json(tag: &str, url: &str, prerelease: bool, draft: bool) -> Vec<u8> {
        serde_json::json!({
            "tag_name": tag,
            "html_url": url,
            "prerelease": prerelease,
            "draft": draft,
            "name": "ignored",
            "assets": [{"name": "calcraft.tar.gz"}],
            "body": "Release notes"
        })
        .to_string()
        .into_bytes()
    }

    const PAGE: &str = "https://github.com/Zeitgleich/CalCraft/releases/tag/v9.0.0";

    #[test]
    fn release_json_is_parsed_defensively() {
        let release = parse_release(&release_json("v9.0.0", PAGE, false, false))
            .unwrap()
            .unwrap();
        assert_eq!(release.version, v(9, 0, 0));
        assert_eq!(release.url.as_deref(), Some(PAGE));

        // Foreign links are dropped, the version is kept.
        let foreign = parse_release(&release_json(
            "v9.0.0",
            "https://evil.example/",
            false,
            false,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(foreign.url, None);

        // Drafts, pre-releases and odd tags are ignored.
        for body in [
            release_json("v9.0.0", PAGE, true, false),
            release_json("v9.0.0", PAGE, false, true),
            release_json("v9.0.0-rc1", PAGE, false, false),
            release_json("nightly", PAGE, false, false),
            release_json(&format!("v{}", "9".repeat(80)), PAGE, false, false),
            br#"{"html_url": "https://github.com/Zeitgleich/CalCraft/releases/x"}"#.to_vec(),
            b"{}".to_vec(),
        ] {
            assert_eq!(parse_release(&body), Ok(None));
        }

        // Malformed input is an error whose message does not quote it.
        for body in [
            b"".to_vec(),
            b"not json".to_vec(),
            b"[1, 2]".to_vec(),
            br#"{"tag_name": 5}"#.to_vec(),
            br#"{"tag_name": "v1.0.0", "draft": "zqsecret"}"#.to_vec(),
            br#"{"tag_name": "v1.0.0""#.to_vec(),
            vec![0xff, 0xfe, 0x00],
        ] {
            let err = parse_release(&body).unwrap_err();
            assert!(matches!(err, ReleaseError::Malformed { .. }));
            assert!(!err.to_string().contains("zqsecret"));
        }

        // Oversized bodies are refused before parsing.
        let mut huge = release_json("v9.0.0", PAGE, false, false);
        huge.truncate(huge.len() - 1);
        huge.extend_from_slice(b",\"padding\":\"");
        huge.resize(MAX_RESPONSE + 1, b'x');
        assert_eq!(parse_release(&huge), Err(ReleaseError::TooLarge));
    }

    #[test]
    fn state_reports_newer_releases_and_keeps_results_on_errors() {
        let check = UpdateCheck::new();
        let info = check.info();
        assert!(info.enabled && !info.newer);
        assert_eq!(info.current, calcraft_core::VERSION);
        assert_eq!((info.latest, info.url, info.checked_at), (None, None, None));

        check
            .apply(&release_json("v9999.0.0", PAGE, false, false), 100)
            .unwrap();
        let info = check.info();
        assert!(info.newer);
        assert_eq!(info.latest.as_deref(), Some("9999.0.0"));
        assert_eq!(info.url.as_deref(), Some(PAGE));
        assert_eq!(info.checked_at, Some(100));

        assert!(check.apply(b"garbage", 200).is_err());
        assert_eq!(check.info().checked_at, Some(100));

        check
            .apply(&release_json("v0.0.1", PAGE, false, false), 300)
            .unwrap();
        let info = check.info();
        assert!(!info.newer);
        assert_eq!(info.latest.as_deref(), Some("0.0.1"));
    }

    #[test]
    fn jitter_is_bounded() {
        let interval = Duration::from_secs(3600);
        for _ in 0..100 {
            let delay = with_jitter(interval);
            assert!(delay >= interval && delay <= interval + Duration::from_secs(360));
        }
        assert_eq!(with_jitter(Duration::ZERO), Duration::ZERO);
    }
}
