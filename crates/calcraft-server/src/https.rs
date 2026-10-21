//! Outbound HTTPS client, shared by single sign-on (`oidc`) and the
//! optional update check (`update-check`).
//!
//! Redirects are not followed, every request has a deadline and a response
//! size limit, and no cookies or credentials are kept between requests (the
//! client has no cookie store). TLS is rustls with the `ring` provider and
//! the operating system's trust store (plus an optional CA file). Callers
//! decide which URLs may be requested.

use std::{path::Path, sync::Arc, time::Duration};

use axum::http::{
    HeaderValue, Method, Request, StatusCode, Uri,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use rustls::{ClientConfig, RootCertStore, pki_types::CertificateDer, pki_types::pem::PemObject};

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
                .map_err(|err| {
                    anyhow::anyhow!("failed to read CA file {}: {err}", path.display())
                })?;
            if certs.is_empty() {
                anyhow::bail!("CA file {} contains no certificates", path.display());
            }
            for cert in certs {
                roots.add(cert)?;
            }
        }
        if roots.is_empty() && !allow_http {
            anyhow::bail!(
                "no trusted CA certificates found in the system trust store (for OIDC: set oidc.ca_file)"
            );
        }
        let tls =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_root_certificates(roots)
                .with_no_client_auth();

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
        .header(
            USER_AGENT,
            HeaderValue::from_static(concat!("calcraft/", env!("CARGO_PKG_VERSION"))),
        )
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
