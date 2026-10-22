//! Fetching a feed: DNS resolution, address checks, pinned connection,
//! manual redirects, conditional requests and limits.

use std::{
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use axum::http::{
    HeaderMap, HeaderValue, StatusCode,
    header::{
        ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE,
        IF_NONE_MATCH, LAST_MODIFIED, LOCATION,
    },
};
use bytes::Bytes;
use rustls::ClientConfig;

use super::{
    SubscriptionError,
    ssrf::{FeedUrl, Host, Policy},
};
use crate::https::{HttpError, PinnedRequest, get_pinned};

/// Deadline for one DNS lookup.
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for establishing one connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Most addresses considered per lookup.
const MAX_ADDRESSES: usize = 32;
/// Longest stored `ETag` / `Last-Modified` value.
const MAX_VALIDATOR_LEN: usize = 256;

/// Future returned by [`Resolver::lookup`].
pub type Lookup<'a> = Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>>;

/// Resolves host names. The system resolver in production; tests inject
/// their own (DNS rebinding tests).
pub trait Resolver: Send + Sync {
    fn lookup<'a>(&'a self, host: &'a str, port: u16) -> Lookup<'a>;
}

/// The operating system's resolver (`getaddrinfo`).
#[derive(Debug, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn lookup<'a>(&'a self, host: &'a str, port: u16) -> Lookup<'a> {
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host, port)).await?;
            Ok(addrs.map(|addr| addr.ip()).collect())
        })
    }
}

/// Validators of the last good response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conditional {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Result of a successful fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// `304 Not Modified`.
    NotModified,
    Body {
        body: Bytes,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// Fetches feeds under a [`Policy`].
#[derive(Clone)]
pub struct Fetcher {
    pub(crate) policy: Policy,
    resolver: Arc<dyn Resolver>,
    /// `None` if no trusted certificates are available (https fails).
    tls: Option<Arc<ClientConfig>>,
    max_bytes: usize,
    timeout: Duration,
    max_redirects: u8,
}

impl std::fmt::Debug for Fetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fetcher")
            .field("policy", &self.policy)
            .field("max_bytes", &self.max_bytes)
            .field("timeout", &self.timeout)
            .field("max_redirects", &self.max_redirects)
            .finish_non_exhaustive()
    }
}

/// A validator header worth keeping: short visible ASCII.
fn validator(headers: &HeaderMap, name: axum::http::HeaderName) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    (!value.is_empty()
        && value.len() <= MAX_VALIDATOR_LEN
        && value.bytes().all(|b| (0x20..0x7f).contains(&b)))
    .then(|| value.to_owned())
}

/// Content types a feed may be served with (iCalendar is parsed in any case).
fn acceptable_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(CONTENT_TYPE) else {
        return true;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let media = value.split(';').next().unwrap_or_default().trim();
    [
        "text/calendar",
        "text/plain",
        "application/octet-stream",
        "application/ics",
        "text/x-vcalendar",
        "application/x-ical",
    ]
    .iter()
    .any(|t| media.eq_ignore_ascii_case(t))
}

/// Whether the body starts like an iCalendar stream.
pub(crate) fn looks_like_icalendar(body: &[u8]) -> bool {
    let body = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
    let start = body
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(body.len());
    let prefix = &body[start..];
    prefix.len() >= 15 && prefix[..15].eq_ignore_ascii_case(b"BEGIN:VCALENDAR")
}

fn header_value(value: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(value).ok()
}

impl Fetcher {
    pub fn new(
        policy: Policy,
        resolver: Arc<dyn Resolver>,
        tls: Option<Arc<ClientConfig>>,
        max_bytes: usize,
        timeout: Duration,
        max_redirects: u8,
    ) -> Self {
        Self {
            policy,
            resolver,
            tls,
            max_bytes,
            timeout,
            max_redirects,
        }
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Fetches `url`, following up to `max_redirects` redirects, each one
    /// checked like the original URL. The whole exchange has one deadline.
    pub async fn fetch(
        &self,
        url: &FeedUrl,
        conditional: &Conditional,
    ) -> Result<Fetched, SubscriptionError> {
        tokio::time::timeout(self.timeout, self.fetch_chain(url, conditional))
            .await
            .unwrap_or(Err(SubscriptionError::FetchFailed(Some("timeout"))))
    }

    async fn fetch_chain(
        &self,
        url: &FeedUrl,
        conditional: &Conditional,
    ) -> Result<Fetched, SubscriptionError> {
        let mut current = url.clone();
        let mut hops = 0u8;
        loop {
            let addr = self.resolve(&current).await?;
            let response = self.request(&current, addr, conditional).await?;
            match response.status {
                StatusCode::OK => {
                    if response
                        .headers
                        .get(CONTENT_ENCODING)
                        .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
                    {
                        return Err(SubscriptionError::FetchFailed(Some("compressed response")));
                    }
                    if !acceptable_type(&response.headers) || !looks_like_icalendar(&response.body)
                    {
                        return Err(SubscriptionError::NotICalendar);
                    }
                    return Ok(Fetched::Body {
                        etag: validator(&response.headers, ETAG),
                        last_modified: validator(&response.headers, LAST_MODIFIED),
                        body: response.body,
                    });
                }
                StatusCode::NOT_MODIFIED
                    if conditional.etag.is_some() || conditional.last_modified.is_some() =>
                {
                    return Ok(Fetched::NotModified);
                }
                StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT => {
                    if hops >= self.max_redirects {
                        return Err(SubscriptionError::FetchFailed(Some("too many redirects")));
                    }
                    hops += 1;
                    let location = response
                        .headers
                        .get(LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .ok_or(SubscriptionError::FetchFailed(Some("invalid redirect")))?;
                    let next = self.policy.redirect(&current, location)?;
                    tracing::debug!(
                        from = %current.host_name(),
                        to = %next.host_name(),
                        "subscription feed redirected"
                    );
                    current = next;
                }
                status => return Err(SubscriptionError::HttpStatus(status.as_u16())),
            }
        }
    }

    /// Resolves the host once and checks every address; returns the one to
    /// connect to. A name with any non-public address is refused, so a
    /// resolver cannot hide a private address behind a public one.
    async fn resolve(&self, url: &FeedUrl) -> Result<SocketAddr, SubscriptionError> {
        let ips = match &url.host {
            Host::Ip(ip) => vec![*ip],
            Host::Name(name) => {
                let lookup = self.resolver.lookup(name, url.port);
                match tokio::time::timeout(DNS_TIMEOUT, lookup).await {
                    Ok(Ok(ips)) => ips,
                    Ok(Err(_)) => {
                        return Err(SubscriptionError::FetchFailed(Some("host not found")));
                    }
                    Err(_) => return Err(SubscriptionError::FetchFailed(Some("DNS timeout"))),
                }
            }
        };
        if ips.is_empty() {
            return Err(SubscriptionError::FetchFailed(Some("host not found")));
        }
        let mut checked = Vec::with_capacity(ips.len().min(MAX_ADDRESSES));
        for ip in ips.into_iter().take(MAX_ADDRESSES) {
            match self.policy.check_ip(ip) {
                Ok(ip) => checked.push(ip),
                Err(err) => {
                    tracing::info!(
                        host = %url.host_name(),
                        "subscription feed refused: host resolves to a non-public address"
                    );
                    return Err(err);
                }
            }
        }
        let ip = checked[0];
        Ok(SocketAddr::new(ip, url.port))
    }

    async fn request(
        &self,
        url: &FeedUrl,
        addr: SocketAddr,
        conditional: &Conditional,
    ) -> Result<crate::https::PinnedResponse, SubscriptionError> {
        let tls = match &self.tls {
            Some(tls) => tls.clone(),
            None => {
                return Err(SubscriptionError::FetchFailed(Some(
                    "no trusted certificates",
                )));
            }
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("text/calendar, text/plain;q=0.8, */*;q=0.1"),
        );
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
        if let Some(value) = conditional.etag.as_deref().and_then(header_value) {
            headers.insert(IF_NONE_MATCH, value);
        }
        if let Some(value) = conditional.last_modified.as_deref().and_then(header_value) {
            headers.insert(IF_MODIFIED_SINCE, value);
        }
        let server_name = url.server_name();
        let host_header = url.host_header();
        let request = PinnedRequest {
            https: url.https,
            addr,
            server_name: &server_name,
            host_header: &host_header,
            path_and_query: &url.path_and_query,
            headers,
            max_body: self.max_bytes,
            connect_timeout: CONNECT_TIMEOUT,
        };
        get_pinned(&tls, request).await.map_err(|err| match err {
            HttpError::TooLarge => SubscriptionError::TooLarge,
            HttpError::Timeout => SubscriptionError::FetchFailed(Some("timeout")),
            err => {
                // The error chain names the address at most, never the path.
                tracing::debug!(host = %url.host_name(), %err, "subscription fetch failed");
                SubscriptionError::FetchFailed(None)
            }
        })
    }
}
