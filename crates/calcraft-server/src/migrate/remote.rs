//! The small CalDAV/CardDAV client behind `calcraft migrate`: requests to
//! exactly one origin, redirects re-checked, responses parsed with the
//! single XML parser of CalCraft.

use std::time::Duration;

use axum::http::{
    HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri,
    header::{AUTHORIZATION, CONTENT_TYPE, LOCATION},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use calcraft_core::CollectionKind;
use calcraft_dav::xml::{NS_CALDAV, NS_CARDDAV, NS_DAV, escape_text};
use calcraft_ical::xml::{self, Node, XmlError, XmlLimits};

use zeroize::Zeroizing;

use crate::{
    https::{HttpClient, HttpError, Limits},
    transfer::printable,
};

/// The source password: wiped from memory when dropped, never printed.
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

/// A URL for messages: without user info, query and fragment, printable.
pub fn redact_url(text: &str) -> String {
    let text = text.trim();
    let text = &text[..text.find(['?', '#']).unwrap_or(text.len())];
    let redacted = match text.find("://") {
        Some(scheme_end) => {
            let rest = &text[scheme_end + 3..];
            let authority_end = rest.find('/').unwrap_or(rest.len());
            let (authority, path) = rest.split_at(authority_end);
            let host = authority.rsplit('@').next().unwrap_or(authority);
            format!("{}{host}{path}", &text[..scheme_end + 3])
        }
        None => text.to_owned(),
    };
    printable(&redacted)
}

const MAX_REDIRECTS: usize = 5;
const XML_MAX_DEPTH: usize = 32;
const XML_MAX_NODES: usize = 1_000_000;
const DEPTH: HeaderName = HeaderName::from_static("depth");

/// Why a request to the source server failed.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("{0}")]
    Invalid(String),
    #[error("the source server rejected the credentials (check --source-user and the password)")]
    Unauthorized,
    #[error("{method} {path}: unexpected status {status}")]
    Status {
        method: Method,
        path: String,
        status: StatusCode,
    },
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(
        "refused redirect to {0}: only the same origin (or http to https on the same host and port) is followed"
    )]
    Redirect(String),
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("invalid XML from the source server: {0}")]
    Xml(#[from] XmlError),
}

/// Scheme, host and port: credentials are only ever sent here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    https: bool,
    host: String,
    port: u16,
}

impl Origin {
    pub fn of(uri: &Uri) -> Result<Self, RemoteError> {
        let https = match uri.scheme_str() {
            Some("https") => true,
            Some("http") => false,
            _ => {
                return Err(RemoteError::Invalid(
                    "the URL must start with https:// (or http:// with --allow-http)".to_owned(),
                ));
            }
        };
        let authority = uri
            .authority()
            .ok_or_else(|| RemoteError::Invalid("the URL has no host".to_owned()))?;
        if authority.as_str().contains('@') {
            return Err(RemoteError::Invalid(
                "user names or passwords in the URL are not accepted: use --source-user and \
                 enter the password at the prompt or with --password-file"
                    .to_owned(),
            ));
        }
        Ok(Self {
            https,
            host: authority.host().to_ascii_lowercase(),
            port: authority.port_u16().unwrap_or(if https { 443 } else { 80 }),
        })
    }

    pub fn is_https(&self) -> bool {
        self.https
    }

    /// `https://host:port`, the prefix of every URL requested.
    pub fn base(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        format!("{scheme}://{}:{}", self.host, self.port)
    }

    /// The only cross-origin redirect followed: the same host and port from
    /// http to https, or the default port 80 to the default port 443.
    /// Credentials then only travel encrypted. (A redirect to another port,
    /// e.g. Synology's 5000 to 5001, is refused: give the https URL.)
    pub fn is_upgrade_to(&self, other: &Self) -> bool {
        !self.https
            && other.https
            && self.host == other.host
            && (self.port == other.port || (self.port == 80 && other.port == 443))
    }
}

/// A `<d:response>` of a multistatus.
#[derive(Debug)]
pub struct Entry {
    pub href: String,
    /// Status of the whole response (`<d:status>` instead of propstats).
    pub status: Option<u16>,
    /// Properties from propstats with a 2xx status.
    pub props: Vec<Node>,
}

impl Entry {
    pub fn prop(&self, namespace: &str, name: &str) -> Option<&Node> {
        self.props.iter().find(|p| p.is(namespace, name))
    }

    /// Text of a property, trimmed; `None` if missing or empty.
    pub fn text(&self, namespace: &str, name: &str) -> Option<&str> {
        self.prop(namespace, name)
            .map(|p| p.text.trim())
            .filter(|t| !t.is_empty())
    }

    /// The `<d:href>` inside a property, e.g. `current-user-principal`.
    pub fn href_prop(&self, namespace: &str, name: &str) -> Option<&str> {
        self.prop(namespace, name)?
            .child(NS_DAV, "href")
            .map(|h| h.text.trim())
            .filter(|t| !t.is_empty())
    }
}

fn status_code(text: &str) -> Option<u16> {
    text.split_whitespace().nth(1)?.parse().ok()
}

/// Parses a `207 Multi-Status` body.
pub fn multistatus(body: &[u8], max_len: usize) -> Result<Vec<Entry>, RemoteError> {
    let limits = XmlLimits {
        max_len,
        max_depth: XML_MAX_DEPTH,
        max_nodes: XML_MAX_NODES,
    };
    let root = xml::parse(body, &limits)?;
    if !root.is(NS_DAV, "multistatus") {
        return Err(RemoteError::Invalid(
            "the source server did not answer with a multistatus".to_owned(),
        ));
    }
    let mut entries = Vec::new();
    for response in root.children_named(NS_DAV, "response") {
        let Some(href) = response.child(NS_DAV, "href") else {
            continue;
        };
        let mut props = Vec::new();
        for propstat in response.children_named(NS_DAV, "propstat") {
            let ok = propstat
                .child(NS_DAV, "status")
                .and_then(|s| status_code(&s.text))
                .is_some_and(|code| (200..300).contains(&code));
            if let (true, Some(prop)) = (ok, propstat.child(NS_DAV, "prop")) {
                props.extend(prop.children.iter().cloned());
            }
        }
        entries.push(Entry {
            href: href.text.trim().to_owned(),
            status: response
                .child(NS_DAV, "status")
                .and_then(|s| status_code(&s.text)),
            props,
        });
    }
    Ok(entries)
}

/// Decodes `%XX` escapes; invalid escapes and non-UTF-8 results are kept
/// as they are. Used to compare paths and to derive slugs.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(&a), Some(&b)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(a), Some(b)) = (hex(a), hex(b))
        {
            // Two hex digits are at most 0xff.
            out.push(u8::try_from(a * 16 + b).unwrap_or(b'?'));
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// A path for comparisons: decoded, without trailing slashes.
pub fn path_key(uri: &Uri) -> String {
    percent_decode(uri.path()).trim_end_matches('/').to_owned()
}

/// A client for one source server and one account.
pub struct Remote {
    http: HttpClient,
    origin: Origin,
    /// `Basic …`, wiped on drop. The `HeaderValue` built from it for each
    /// request is marked sensitive and lives only as long as the request.
    authorization: Zeroizing<String>,
    limits: Limits,
    delay: Duration,
    /// Set once the server redirected from http to https.
    pub upgraded: bool,
}

impl Remote {
    pub fn new(
        http: HttpClient,
        start: &Uri,
        user: &str,
        password: &Password,
        limits: Limits,
        delay: Duration,
    ) -> Result<Self, RemoteError> {
        let origin = Origin::of(start)?;
        // Buffers are allocated with their final size, so no copy is left
        // behind by reallocation.
        let password = password.expose();
        let mut plain = Zeroizing::new(String::with_capacity(user.len() + 1 + password.len()));
        plain.push_str(user);
        plain.push(':');
        plain.push_str(password);
        let encoded = plain.len().div_ceil(3) * 4;
        let mut authorization = Zeroizing::new(String::with_capacity(6 + encoded));
        authorization.push_str("Basic ");
        STANDARD.encode_string(plain.as_bytes(), &mut authorization);
        let remote = Self {
            http,
            origin,
            authorization,
            limits,
            delay,
            upgraded: false,
        };
        remote.authorization_header()?;
        Ok(remote)
    }

    fn authorization_header(&self) -> Result<HeaderValue, RemoteError> {
        let mut value = HeaderValue::from_str(&self.authorization)
            .map_err(|_| RemoteError::Invalid("invalid user name".to_owned()))?;
        value.set_sensitive(true);
        Ok(value)
    }

    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Resolves an href or redirect target against `base`. `None` for
    /// malformed references and for other origins.
    pub fn resolve(&self, base: &Uri, href: &str) -> Option<Uri> {
        let href = href.trim().replace(' ', "%20");
        if href.is_empty() {
            return None;
        }
        let uri: Uri = if href.starts_with("http://") || href.starts_with("https://") {
            let uri: Uri = href.parse().ok()?;
            let origin = Origin::of(&uri).ok()?;
            if origin != self.origin {
                return None;
            }
            format!(
                "{}{}",
                self.origin.base(),
                uri.path_and_query().map_or("/", |p| p.as_str())
            )
            .parse()
            .ok()?
        } else if href.starts_with('/') {
            format!("{}{href}", self.origin.base()).parse().ok()?
        } else {
            let path = base.path();
            let dir = &path[..=path.rfind('/')?];
            format!("{}{dir}{href}", self.origin.base()).parse().ok()?
        };
        // Only plain paths: no `..` tricks, no fragments.
        let decoded = percent_decode(uri.path());
        if decoded.split('/').any(|segment| segment == "..") {
            return None;
        }
        Some(uri)
    }

    /// After an upgrade, URLs given for the old http origin go to https.
    fn rebase(&self, uri: &Uri) -> Result<Uri, RemoteError> {
        let origin = Origin::of(uri)?;
        if origin == self.origin {
            return Ok(uri.clone());
        }
        if self.upgraded && origin.is_upgrade_to(&self.origin) {
            return format!(
                "{}{}",
                self.origin.base(),
                uri.path_and_query().map_or("/", |p| p.as_str())
            )
            .parse()
            .map_err(|_| RemoteError::Invalid("invalid URL".to_owned()));
        }
        Err(RemoteError::Redirect(origin.base()))
    }

    /// Sends a request, following redirects within the origin. Returns
    /// the final URL (for relative hrefs), status and body.
    pub async fn send(
        &mut self,
        method: Method,
        uri: &Uri,
        depth: Option<&'static str>,
        body: Bytes,
        max_body: usize,
    ) -> Result<(Uri, StatusCode, Bytes), RemoteError> {
        let mut uri = self.rebase(uri)?;
        let mut method = method;
        let mut body = body;
        for _ in 0..=MAX_REDIRECTS {
            let response = self
                .exchange(&method, &uri, depth, body.clone(), max_body)
                .await?;
            let status = response.status;
            if status == StatusCode::UNAUTHORIZED {
                return Err(RemoteError::Unauthorized);
            }
            if !status.is_redirection() || status == StatusCode::NOT_MODIFIED {
                return Ok((uri, status, response.body));
            }
            let location = response
                .headers
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| RemoteError::Invalid("redirect without Location".to_owned()))?;
            uri = self.follow(&uri, location)?;
            if status == StatusCode::SEE_OTHER {
                method = Method::GET;
                body = Bytes::new();
            }
        }
        Err(RemoteError::TooManyRedirects)
    }

    /// Checks a redirect target: same origin, or http to https on the same
    /// host (which then becomes the origin).
    fn follow(&mut self, current: &Uri, location: &str) -> Result<Uri, RemoteError> {
        if let Some(next) = self.resolve(current, location) {
            return Ok(next);
        }
        let refused = || RemoteError::Redirect(redact_url(location));
        let target: Uri = location.trim().parse().map_err(|_| refused())?;
        let origin = Origin::of(&target).map_err(|_| refused())?;
        if self.origin.is_upgrade_to(&origin) {
            tracing::info!(to = %origin.base(), "source server redirected to https");
            self.origin = origin;
            self.upgraded = true;
            return self.resolve(current, location).ok_or_else(refused);
        }
        Err(RemoteError::Redirect(origin.base()))
    }

    async fn exchange(
        &self,
        method: &Method,
        uri: &Uri,
        depth: Option<&'static str>,
        body: Bytes,
        max_body: usize,
    ) -> Result<crate::https::Response, RemoteError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, self.authorization_header()?);
        if let Some(depth) = depth {
            headers.insert(DEPTH, HeaderValue::from_static(depth));
        }
        if !body.is_empty() {
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_static("application/xml; charset=utf-8"),
            );
        }
        let limits = Limits {
            timeout: self.limits.timeout,
            max_body,
        };
        Ok(self
            .http
            .request(method.clone(), uri, headers, body, limits)
            .await?)
    }

    /// PROPFIND; the answer must be a multistatus.
    pub async fn propfind(
        &mut self,
        uri: &Uri,
        depth: &'static str,
        body: &'static str,
    ) -> Result<(Uri, Vec<Entry>), RemoteError> {
        let max = self.limits.max_body;
        let (at, status, body) = self
            .send(
                propfind_method(),
                uri,
                Some(depth),
                Bytes::from_static(body.as_bytes()),
                max,
            )
            .await?;
        if status != StatusCode::MULTI_STATUS {
            return Err(RemoteError::Status {
                method: propfind_method(),
                path: printable(at.path()),
                status,
            });
        }
        Ok((at, multistatus(&body, max)?))
    }

    /// `calendar-multiget` / `addressbook-multiget` (RFC 4791 §7.9,
    /// RFC 6352 §8.7). `None` if the server does not support it.
    pub async fn multiget(
        &mut self,
        collection: &Uri,
        kind: CollectionKind,
        paths: &[String],
    ) -> Result<Option<(Uri, Vec<Entry>)>, RemoteError> {
        let (ns, report, data) = match kind {
            CollectionKind::Calendar => (NS_CALDAV, "calendar-multiget", "calendar-data"),
            CollectionKind::Addressbook => (NS_CARDDAV, "addressbook-multiget", "address-data"),
        };
        let mut body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<x:{report} xmlns:d=\"DAV:\" \
             xmlns:x=\"{ns}\"><d:prop><d:getetag/><x:{data}/></d:prop>"
        );
        for path in paths {
            body.push_str("<d:href>");
            body.push_str(&escape_text(path));
            body.push_str("</d:href>");
        }
        body.push_str("</x:");
        body.push_str(report);
        body.push('>');
        let max = self.limits.max_body;
        let (at, status, response) = self
            .send(
                report_method(),
                collection,
                Some("1"),
                Bytes::from(body),
                max,
            )
            .await?;
        match status {
            StatusCode::MULTI_STATUS => Ok(Some((at, multistatus(&response, max)?))),
            // Unknown report, not allowed, not implemented: fall back to GET.
            status if status.is_client_error() || status == StatusCode::NOT_IMPLEMENTED => Ok(None),
            status => Err(RemoteError::Status {
                method: report_method(),
                path: printable(at.path()),
                status,
            }),
        }
    }

    /// GET of one object; `None` if it is gone.
    pub async fn get(&mut self, uri: &Uri, max_body: usize) -> Result<Option<Bytes>, RemoteError> {
        let (at, status, body) = self
            .send(Method::GET, uri, None, Bytes::new(), max_body)
            .await?;
        match status {
            StatusCode::OK => Ok(Some(body)),
            StatusCode::NOT_FOUND | StatusCode::GONE => Ok(None),
            status => Err(RemoteError::Status {
                method: Method::GET,
                path: printable(at.path()),
                status,
            }),
        }
    }
}

fn propfind_method() -> Method {
    // A valid token, so the conversion cannot fail.
    Method::from_bytes(b"PROPFIND").unwrap_or(Method::OPTIONS)
}

fn report_method() -> Method {
    Method::from_bytes(b"REPORT").unwrap_or(Method::OPTIONS)
}

#[cfg(test)]
mod tests;
