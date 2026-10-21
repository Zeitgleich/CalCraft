//! Which identity provider URLs may be requested.
//!
//! SSRF hardening: only URLs that pass [`EndpointPolicy::check`] are ever
//! requested (the issuer's own host, plus hosts the administrator listed).
//! The client itself (deadline, size limit, no redirects) is
//! [`crate::https`].

use axum::http::Uri;

pub use crate::https::{HttpClient, HttpError};

/// Which URLs may be fetched or used as authorization endpoint.
#[derive(Debug, Clone)]
pub struct EndpointPolicy {
    issuer_host: String,
    issuer_port: u16,
    extra_hosts: Vec<String>,
    insecure: bool,
}

fn effective_port(uri: &Uri) -> Option<u16> {
    uri.port_u16().or(match uri.scheme_str() {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    })
}

impl EndpointPolicy {
    pub fn new(issuer: &Uri, extra_hosts: &[String], insecure: bool) -> Option<Self> {
        Some(Self {
            issuer_host: issuer.host()?.to_ascii_lowercase(),
            issuer_port: effective_port(issuer)?,
            extra_hosts: extra_hosts.iter().map(|h| h.to_ascii_lowercase()).collect(),
            insecure,
        })
    }

    /// Parses an endpoint URL from discovery and checks that it is https
    /// on the issuer's host and port (or on an extra host, port 443). The
    /// testing flag allows any http(s) URL.
    pub fn check(&self, url: &str) -> Result<Uri, HttpError> {
        let uri: Uri = url
            .parse()
            .map_err(|_| HttpError::NotAllowed("invalid URL"))?;
        let authority = uri
            .authority()
            .ok_or(HttpError::NotAllowed("relative URL"))?;
        if authority.as_str().contains('@') || url.contains('#') {
            return Err(HttpError::NotAllowed("user info or fragment"));
        }
        if self.insecure {
            return match uri.scheme_str() {
                Some("https" | "http") => Ok(uri),
                _ => Err(HttpError::NotAllowed("scheme")),
            };
        }
        if uri.scheme_str() != Some("https") {
            return Err(HttpError::NotAllowed("endpoint must use https"));
        }
        let host = authority.host().to_ascii_lowercase();
        let port = effective_port(&uri);
        let allowed = (host == self.issuer_host && port == Some(self.issuer_port))
            || (self.extra_hosts.contains(&host) && port == Some(443));
        if allowed {
            Ok(uri)
        } else {
            Err(HttpError::NotAllowed("endpoint not on the issuer's host"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_must_be_on_the_issuer_host() {
        let issuer: Uri = "https://idp.example/realms/main".parse().unwrap();
        let policy = EndpointPolicy::new(&issuer, &["keys.example".to_owned()], false).unwrap();
        for ok in [
            "https://idp.example/realms/main/protocol/openid-connect/token",
            "https://IDP.example:443/certs",
            "https://keys.example/jwks",
        ] {
            policy.check(ok).unwrap();
        }
        for bad in [
            "http://idp.example/token",
            "https://idp.example:8443/token",
            "https://idp.example.evil.example/token",
            "https://evil.example/token",
            "https://user@idp.example/token",
            "https://169.254.169.254/latest/meta-data",
            "https://keys.example:8443/jwks",
            "/relative",
            "file:///etc/passwd",
        ] {
            assert!(policy.check(bad).is_err(), "{bad}");
        }
        let insecure = EndpointPolicy::new(&issuer, &[], true).unwrap();
        insecure.check("http://127.0.0.1:9/token").unwrap();
        assert!(insecure.check("file:///etc/passwd").is_err());
    }
}
