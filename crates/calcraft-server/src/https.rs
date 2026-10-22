//! Outbound HTTPS client, shared by single sign-on (`oidc`), the optional
//! update check (`update-check`) and calendar subscriptions
//! (`subscriptions`).
//!
//! Redirects are not followed, every request has a deadline and a response
//! size limit, and no cookies or credentials are kept between requests (the
//! client has no cookie store). TLS is rustls with the `ring` provider and
//! the operating system's trust store (plus an optional CA file). Callers
//! decide which URLs may be requested.
//!
//! [`get_pinned`] is the building block of the subscription fetcher: one
//! request over a fresh connection to an address the caller resolved and
//! checked itself, so no second DNS lookup can redirect it (DNS
//! rebinding). [`HttpClient`] (OIDC, update check) is unchanged by it.

use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

use axum::http::{
    HeaderMap, HeaderValue, Method, Request, StatusCode, Uri,
    header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST, USER_AGENT},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper_rustls::{FixedServerNameResolver, HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, ServerName, pem::PemObject},
};

/// Deadline for a complete request including the response body.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest accepted response body (discovery, JWKS, token response,
/// release information).
pub const MAX_RESPONSE: usize = 256 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// A URL rejected by the caller's policy (OIDC endpoint checks).
    #[error("endpoint not allowed: {0}")]
    #[cfg_attr(not(feature = "oidc"), allow(dead_code))]
    NotAllowed(&'static str),
    #[error("request timed out")]
    Timeout,
    #[error("request failed: {0}")]
    Transport(String),
    #[error("unexpected status {0}")]
    Status(StatusCode),
    #[error("response too large or unreadable")]
    Body,
    /// The response body exceeded the caller's limit ([`get_pinned`]).
    #[error("response too large")]
    #[cfg_attr(not(feature = "subscriptions"), allow(dead_code))]
    TooLarge,
}

/// `User-Agent` of every outbound request.
pub const USER_AGENT_VALUE: &str = concat!("calcraft/", env!("CARGO_PKG_VERSION"));

/// TLS settings: the system trust store plus the certificates of
/// `ca_file`. Fails without any trusted certificate unless `allow_empty`.
pub fn tls_config(ca_file: Option<&Path>, allow_empty: bool) -> anyhow::Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    if !native.errors.is_empty() {
        tracing::warn!(
            errors = native.errors.len(),
            "some system CA certificates could not be loaded"
        );
    }
    let (_, ignored) = roots.add_parsable_certificates(native.certs);
    if ignored > 0 {
        tracing::debug!(ignored, "ignored unparsable system CA certificates");
    }
    if let Some(path) = ca_file {
        let certs = CertificateDer::pem_file_iter(path)
            .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
            .map_err(|err| anyhow::anyhow!("failed to read CA file {}: {err}", path.display()))?;
        if certs.is_empty() {
            anyhow::bail!("CA file {} contains no certificates", path.display());
        }
        for cert in certs {
            roots.add(cert)?;
        }
    }
    if roots.is_empty() && !allow_empty {
        anyhow::bail!(
            "no trusted CA certificates found in the system trust store (for OIDC: set oidc.ca_file)"
        );
    }
    Ok(Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

/// HTTP(S) client with deadline and size limit.
#[derive(Clone)]
pub struct HttpClient {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HttpClient")
    }
}

impl HttpClient {
    /// `allow_http` only for tests with a local mock server.
    pub fn new(ca_file: Option<&Path>, allow_http: bool) -> anyhow::Result<Self> {
        let tls = Arc::unwrap_or_clone(tls_config(ca_file, allow_http)?);

        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_connect_timeout(Some(CONNECT_TIMEOUT));
        let builder = HttpsConnectorBuilder::new().with_tls_config(tls);
        let builder = if allow_http {
            builder.https_or_http()
        } else {
            builder.https_only()
        };
        let connector = builder.enable_http1().wrap_connector(http);
        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(30))
            .build(connector);
        Ok(Self { client })
    }

    /// GET with `Accept: application/json`; returns the body of a 200 response.
    pub async fn get(&self, uri: &Uri) -> Result<Bytes, HttpError> {
        let request = base_request(Method::GET, uri)
            .body(Full::new(Bytes::new()))
            .map_err(|err| HttpError::Transport(err.to_string()))?;
        self.send(request).await.and_then(|(status, body)| {
            if status == StatusCode::OK {
                Ok(body)
            } else {
                Err(HttpError::Status(status))
            }
        })
    }

    /// POST of an `application/x-www-form-urlencoded` body; returns status
    /// and body (error responses of the token endpoint carry JSON).
    #[cfg_attr(not(feature = "oidc"), allow(dead_code))]
    pub async fn post_form(
        &self,
        uri: &Uri,
        form: String,
        authorization: Option<HeaderValue>,
    ) -> Result<(StatusCode, Bytes), HttpError> {
        let mut builder = base_request(Method::POST, uri).header(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        if let Some(mut value) = authorization {
            value.set_sensitive(true);
            builder = builder.header(AUTHORIZATION, value);
        }
        let request = builder
            .body(Full::new(Bytes::from(form)))
            .map_err(|err| HttpError::Transport(err.to_string()))?;
        self.send(request).await
    }

    async fn send(&self, request: Request<Full<Bytes>>) -> Result<(StatusCode, Bytes), HttpError> {
        let exchange = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|err| HttpError::Transport(error_chain(&err)))?;
            let status = response.status();
            let body = Limited::new(response.into_body(), MAX_RESPONSE)
                .collect()
                .await
                .map_err(|_| HttpError::Body)?
                .to_bytes();
            Ok((status, body))
        };
        tokio::time::timeout(REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| HttpError::Timeout)?
    }
}

fn base_request(method: Method, uri: &Uri) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri.clone())
        .header(ACCEPT, HeaderValue::from_static("application/json"))
        .header(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE))
}

/// One `GET` over a fresh connection to a pre-checked address.
#[cfg_attr(not(feature = "subscriptions"), allow(dead_code))]
#[derive(Debug)]
pub struct PinnedRequest<'a> {
    /// TLS (`https`) or plain HTTP.
    pub https: bool,
    /// The only address connected to; no DNS lookup happens.
    pub addr: SocketAddr,
    /// Name the server certificate must be valid for (and SNI): the host of
    /// the URL, IPv6 addresses without brackets.
    pub server_name: &'a str,
    /// `Host` header (host, plus port if not the default).
    pub host_header: &'a str,
    /// Path and query, starting with `/`.
    pub path_and_query: &'a str,
    /// Further request headers; `User-Agent` and `Host` are set here.
    pub headers: HeaderMap,
    /// Largest accepted body; longer bodies fail with [`HttpError::TooLarge`].
    pub max_body: usize,
    pub connect_timeout: Duration,
}

/// Status, headers and (for 200 only) body of a pinned request.
#[cfg_attr(not(feature = "subscriptions"), allow(dead_code))]
#[derive(Debug)]
pub struct PinnedResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// Sends `request` to exactly `request.addr` and reads the response. The
/// connection is not pooled (a pooled connection could be reused for
/// another host name on the same address). The caller sets the overall
/// deadline. Bodies of responses other than 200 are not read.
#[cfg_attr(not(feature = "subscriptions"), allow(dead_code))]
pub async fn get_pinned(
    tls: &Arc<ClientConfig>,
    request: PinnedRequest<'_>,
) -> Result<PinnedResponse, HttpError> {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_connect_timeout(Some(request.connect_timeout));
    http.set_nodelay(true);
    let server_name = ServerName::try_from(request.server_name.to_owned())
        .map_err(|_| HttpError::Transport("invalid server name".to_owned()))?;
    let builder = HttpsConnectorBuilder::new().with_tls_config(ClientConfig::clone(tls));
    let builder = if request.https {
        builder.https_only()
    } else {
        builder.https_or_http()
    };
    let connector = builder
        .with_server_name_resolver(FixedServerNameResolver::new(server_name))
        .enable_http1()
        .wrap_connector(http);
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(0)
        .build(connector);

    // The URI names the checked address itself, so the connector never
    // resolves anything; Host and SNI carry the real name.
    let scheme = if request.https { "https" } else { "http" };
    let uri: Uri = format!("{scheme}://{}{}", request.addr, request.path_and_query)
        .parse()
        .map_err(|_| HttpError::Transport("invalid request URI".to_owned()))?;
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    if let Some(headers) = builder.headers_mut() {
        *headers = request.headers;
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        headers.insert(
            HOST,
            HeaderValue::from_str(request.host_header)
                .map_err(|_| HttpError::Transport("invalid host".to_owned()))?,
        );
    }
    let outgoing = builder
        .body(Full::new(Bytes::new()))
        .map_err(|err| HttpError::Transport(err.to_string()))?;
    let response = client
        .request(outgoing)
        .await
        .map_err(|err| HttpError::Transport(error_chain(&err)))?;
    let status = response.status();
    let headers = response.headers().clone();
    if status != StatusCode::OK {
        return Ok(PinnedResponse {
            status,
            headers,
            body: Bytes::new(),
        });
    }
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > request.max_body as u64) {
        return Err(HttpError::TooLarge);
    }
    let body = Limited::new(response.into_body(), request.max_body)
        .collect()
        .await
        .map_err(|err| {
            if err
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some()
            {
                HttpError::TooLarge
            } else {
                HttpError::Body
            }
        })?
        .to_bytes();
    Ok(PinnedResponse {
        status,
        headers,
        body,
    })
}

/// Error message including sources (connection refused, TLS errors, …).
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(err) = source {
        message.push_str(": ");
        message.push_str(&err.to_string());
        source = err.source();
    }
    message
}
