//! End-to-end SSO tests against a tiny in-process mock IdP (axum on
//! 127.0.0.1, plain HTTP via `allow_insecure_issuer_for_testing`).

#![cfg(feature = "oidc")]
#![allow(clippy::unwrap_used)]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode, header},
    routing::{get, post},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use calcraft_server::{
    AppState, Config,
    auth::hash_password,
    config::{OidcConfig, Secret},
    oidc::Oidc,
    router,
};
use calcraft_store::Store;
use http_body_util::BodyExt;
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower::ServiceExt;

const CLIENT_ID: &str = "calcraft";
const CLIENT_SECRET: &str = "test-client-secret";
const REDIRECT_URI: &str = "http://localhost/api/v1/auth/oidc/callback";

struct Key {
    kid: String,
    pair: EcdsaKeyPair,
}

impl Key {
    fn new(kid: &str) -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        Self {
            kid: kid.to_owned(),
            pair: EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap(),
        }
    }

    fn jwk(&self) -> Value {
        let point = self.pair.public_key().as_ref();
        json!({"kty": "EC", "crv": "P-256", "kid": self.kid, "use": "sig", "alg": "ES256",
               "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
               "y": URL_SAFE_NO_PAD.encode(&point[33..65])})
    }

    fn sign(&self, claims: &Value) -> String {
        let header = json!({"alg": "ES256", "kid": self.kid, "typ": "JWT"});
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let sig = self
            .pair
            .sign(&SystemRandom::new(), input.as_bytes())
            .unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
    }
}

/// An authorization the mock IdP has granted: redeemable once.
struct Grant {
    challenge: String,
    claims: Value,
}

#[derive(Default)]
struct IdpState {
    issuer: String,
    /// Keys published in the JWKS; the last one signs.
    keys: Vec<Key>,
    grants: HashMap<String, Grant>,
    jwks_fetches: usize,
    token_requests: usize,
}

type Shared = Arc<Mutex<IdpState>>;

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (percent_decode(k), percent_decode(v)))
        .collect()
}

async fn discovery(State(idp): State<Shared>) -> Json<Value> {
    let issuer = idp.lock().unwrap().issuer.clone();
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/auth"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
        "id_token_signing_alg_values_supported": ["ES256"],
    }))
}

async fn jwks(State(idp): State<Shared>) -> Json<Value> {
    let mut idp = idp.lock().unwrap();
    idp.jwks_fetches += 1;
    Json(json!({ "keys": idp.keys.iter().map(Key::jwk).collect::<Vec<_>>() }))
}

async fn token(
    State(idp): State<Shared>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let mut idp = idp.lock().unwrap();
    idp.token_requests += 1;
    let form = parse_query(std::str::from_utf8(&body).unwrap());
    let basic = format!(
        "Basic {}",
        STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
    );
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(&basic)
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_client"})),
        );
    }
    if form.get("grant_type").map(String::as_str) != Some("authorization_code")
        || form.get("redirect_uri").map(String::as_str) != Some(REDIRECT_URI)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_request"})),
        );
    }
    // Codes are single use.
    let Some(grant) = form.get("code").and_then(|code| idp.grants.remove(code)) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        );
    };
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ));
    if challenge != grant.challenge {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        );
    }
    let id_token = idp.keys.last().unwrap().sign(&grant.claims);
    (
        StatusCode::OK,
        Json(
            json!({"access_token": "opaque", "token_type": "Bearer", "expires_in": 300, "id_token": id_token}),
        ),
    )
}

struct Idp {
    state: Shared,
    issuer: String,
}

impl Idp {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let state: Shared = Arc::new(Mutex::new(IdpState {
            issuer: issuer.clone(),
            keys: vec![Key::new("k1")],
            ..IdpState::default()
        }));
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { state, issuer }
    }

    fn config(&self) -> OidcConfig {
        OidcConfig {
            enabled: true,
            issuer: self.issuer.clone(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: Some(Secret::new(CLIENT_SECRET)),
            redirect_uri: REDIRECT_URI.to_owned(),
            allow_insecure_issuer_for_testing: true,
            ..OidcConfig::default()
        }
    }

    /// Simulates the user logging in at the IdP: returns a code for the
    /// authorization request, with these claims in the ID token.
    fn authorize(&self, request: &HashMap<String, String>, claims: Value) -> String {
        let code = URL_SAFE_NO_PAD.encode(calcraft_server::secrets::random_bytes::<16>());
        self.state.lock().unwrap().grants.insert(
            code.clone(),
            Grant {
                challenge: request["code_challenge"].clone(),
                claims,
            },
        );
        code
    }

    fn claims(&self, request: &HashMap<String, String>, username: &str, sub: &str) -> Value {
        let now = calcraft_server::auth::now();
        json!({
            "iss": self.issuer, "aud": CLIENT_ID, "azp": CLIENT_ID, "sub": sub,
            "nonce": request["nonce"], "iat": now, "exp": now + 300,
            "preferred_username": username, "email": "admin@example.com",
            "name": "Some User", "groups": ["/staff"],
        })
    }
}

async fn app_with(
    idp: &Idp,
    config: impl FnOnce(&mut OidcConfig),
    jwks_refresh: Duration,
) -> Router {
    let store = Store::in_memory().await.unwrap();
    let hash = hash_password("alice password 123").unwrap();
    store
        .create_user(&"alice".parse().unwrap(), &hash, None)
        .await
        .unwrap();
    store
        .create_user(&"admin".parse().unwrap(), &hash, None)
        .await
        .unwrap();
    store
        .set_admin(&"admin".parse().unwrap(), true)
        .await
        .unwrap();
    let mut oidc = idp.config();
    config(&mut oidc);
    let mut state = AppState::new(store).unwrap();
    state.oidc = Some(Oidc::with_jwks_min_refresh(&oidc, jwks_refresh).unwrap());
    router(&Config::default(), state)
}

async fn app(idp: &Idp) -> Router {
    app_with(idp, |_| {}, Duration::from_secs(60)).await
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    json: Value,
}

impl Reply {
    fn location(&self) -> &str {
        self.headers[header::LOCATION].to_str().unwrap()
    }

    /// `name=value` of a Set-Cookie header.
    fn cookie(&self, name: &str) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with(&format!("{name}=")) && !v.contains("Max-Age=0"))
            .map(|v| v.split(';').next().unwrap().to_owned())
    }
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-calcraft-request", "1");
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

/// Starts a login; returns the authorization request parameters and the
/// browser binding cookie.
async fn start(app: &Router, idp: &Idp) -> (HashMap<String, String>, String) {
    let reply = call(app, Method::GET, "/api/v1/auth/oidc/login", None, None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let location = reply.location().to_owned();
    let (endpoint, query) = location.split_once('?').unwrap();
    assert_eq!(endpoint, format!("{}/auth", idp.issuer));
    let binding = reply.cookie("calcraft_oidc").unwrap();
    (parse_query(query), binding)
}

async fn callback(app: &Router, state: &str, code: &str, cookie: Option<&str>) -> Reply {
    let uri = format!("/api/v1/auth/oidc/callback?code={code}&state={state}&session_state=x");
    call(app, Method::GET, &uri, cookie, None).await
}

/// Full login as `username` with subject `sub`; returns the callback reply.
async fn login(app: &Router, idp: &Idp, username: &str, sub: &str) -> Reply {
    let (request, binding) = start(app, idp).await;
    let code = idp.authorize(&request, idp.claims(&request, username, sub));
    callback(app, &request["state"], &code, Some(&binding)).await
}

fn assert_failed(reply: &Reply, code: &str) {
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.location(), format!("/?sso_error={code}"));
    assert!(reply.cookie("calcraft_session").is_none());
}

#[tokio::test]
async fn full_login_flow_creates_a_session() {
    let idp = Idp::start().await;
    let app = app(&idp).await;

    let methods = call(&app, Method::GET, "/api/v1/auth/methods", None, None).await;
    assert_eq!(methods.json["password"], true);
    assert_eq!(methods.json["oidc"]["login_url"], "/api/v1/auth/oidc/login");

    let (request, binding) = start(&app, &idp).await;
    assert_eq!(request["response_type"], "code");
    assert_eq!(request["client_id"], CLIENT_ID);
    assert_eq!(request["redirect_uri"], REDIRECT_URI);
    assert_eq!(request["code_challenge_method"], "S256");
    assert!(request["scope"].split(' ').any(|s| s == "openid"));
    assert!(request["state"].len() >= 43 && request["nonce"].len() >= 43);
    assert!(!request.contains_key("code_verifier"));
    assert!(binding.starts_with("calcraft_oidc="));

    let code = idp.authorize(&request, idp.claims(&request, "Alice", "sub-alice"));
    let reply = callback(&app, &request["state"], &code, Some(&binding)).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.location(), "/");
    let session = reply.cookie("calcraft_session").unwrap();
    let cookies: Vec<_> = reply.headers.get_all(header::SET_COOKIE).iter().collect();
    assert!(cookies.iter().any(|c| {
        let c = c.to_str().unwrap();
        c.starts_with("calcraft_oidc=;") && c.contains("Max-Age=0")
    }));

    let me = call(&app, Method::GET, "/api/v1/me", Some(&session), None).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json["username"], "alice");
    assert_eq!(me.json["is_admin"], false);

    // Replaying the same callback (state and code) is rejected.
    let replay = callback(&app, &request["state"], &code, Some(&binding)).await;
    assert_failed(&replay, "invalid_request");
    assert_eq!(idp.state.lock().unwrap().token_requests, 1);

    // The password login of the linked account still works.
    let password = call(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(json!({"username": "alice", "password": "alice password 123"})),
    )
    .await;
    assert_eq!(password.status, StatusCode::OK);
}

#[tokio::test]
async fn state_must_belong_to_this_browser() {
    let idp = Idp::start().await;
    let app = app(&idp).await;

    // Without the binding cookie (e.g. a link sent to the victim).
    let (request, _) = start(&app, &idp).await;
    let code = idp.authorize(&request, idp.claims(&request, "alice", "s"));
    assert_failed(
        &callback(&app, &request["state"], &code, None).await,
        "invalid_request",
    );

    // With another browser's binding cookie.
    let (request, _) = start(&app, &idp).await;
    let (_, other_binding) = start(&app, &idp).await;
    let code = idp.authorize(&request, idp.claims(&request, "alice", "s"));
    let reply = callback(&app, &request["state"], &code, Some(&other_binding)).await;
    assert_failed(&reply, "invalid_request");

    // Unknown or missing state.
    let (_, binding) = start(&app, &idp).await;
    assert_failed(
        &callback(&app, "forged", "code", Some(&binding)).await,
        "invalid_request",
    );
    let reply = call(
        &app,
        Method::GET,
        "/api/v1/auth/oidc/callback?code=x",
        Some(&binding),
        None,
    )
    .await;
    assert_failed(&reply, "invalid_request");

    // No code was ever redeemed.
    assert_eq!(idp.state.lock().unwrap().token_requests, 0);
}

#[tokio::test]
async fn token_problems_are_rejected() {
    let idp = Idp::start().await;
    let app = app(&idp).await;

    // Nonce of another login.
    let (request, binding) = start(&app, &idp).await;
    let mut claims = idp.claims(&request, "alice", "s");
    claims["nonce"] = json!("not-the-nonce");
    let code = idp.authorize(&request, claims);
    assert_failed(
        &callback(&app, &request["state"], &code, Some(&binding)).await,
        "login_failed",
    );

    // Wrong audience, wrong issuer, expired.
    for change in [
        json!({"aud": "other-client", "azp": "other-client"}),
        json!({"iss": "http://evil.example"}),
        json!({"exp": calcraft_server::auth::now() - 120}),
    ] {
        let (request, binding) = start(&app, &idp).await;
        let mut claims = idp.claims(&request, "alice", "s");
        for (key, value) in change.as_object().unwrap() {
            claims[key] = value.clone();
        }
        let code = idp.authorize(&request, claims);
        let reply = callback(&app, &request["state"], &code, Some(&binding)).await;
        assert_failed(&reply, "login_failed");
    }

    // A code the IdP does not know (token endpoint answers 400).
    let (request, binding) = start(&app, &idp).await;
    assert_failed(
        &callback(&app, &request["state"], "bogus", Some(&binding)).await,
        "login_failed",
    );

    // The user cancelled at the IdP.
    let (request, binding) = start(&app, &idp).await;
    let uri = format!(
        "/api/v1/auth/oidc/callback?error=access_denied&state={}",
        request["state"]
    );
    assert_failed(
        &call(&app, Method::GET, &uri, Some(&binding), None).await,
        "denied",
    );

    // RFC 9207 iss parameter of another issuer (mix-up).
    let (request, binding) = start(&app, &idp).await;
    let code = idp.authorize(&request, idp.claims(&request, "alice", "s"));
    let uri = format!(
        "/api/v1/auth/oidc/callback?code={code}&state={}&iss=http%3A%2F%2Fevil.example",
        request["state"]
    );
    assert_failed(
        &call(&app, Method::GET, &uri, Some(&binding), None).await,
        "invalid_request",
    );
}

#[tokio::test]
async fn account_mapping_rules() {
    let idp = Idp::start().await;
    let app = app(&idp).await;

    // Unknown user without auto-creation.
    assert_failed(&login(&app, &idp, "mallory", "sub-m").await, "no_account");

    // The first login binds the subject; another IdP account with the same
    // username is rejected afterwards.
    assert_eq!(login(&app, &idp, "alice", "sub-1").await.location(), "/");
    assert_failed(&login(&app, &idp, "alice", "sub-2").await, "login_failed");
    assert_eq!(login(&app, &idp, "alice", "sub-1").await.location(), "/");
    // The bound subject cannot be used for another account either.
    assert_failed(&login(&app, &idp, "admin", "sub-1").await, "login_failed");

    // Usernames that violate the local rules are rejected, not rewritten.
    assert_failed(
        &login(&app, &idp, "../admin", "sub-x").await,
        "login_failed",
    );

    // Disabled accounts cannot log in.
    let admin_session = call(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(json!({"username": "admin", "password": "alice password 123"})),
    )
    .await
    .cookie("calcraft_session")
    .unwrap();
    let disable = call(
        &app,
        Method::PATCH,
        "/api/v1/admin/users/alice",
        Some(&admin_session),
        Some(json!({"disabled": true})),
    )
    .await;
    assert_eq!(disable.status, StatusCode::NO_CONTENT);
    assert_failed(&login(&app, &idp, "alice", "sub-1").await, "login_failed");
}

#[tokio::test]
async fn auto_created_users_and_admin_claim() {
    let idp = Idp::start().await;
    let app = app_with(
        &idp,
        |c| {
            c.auto_create_users = true;
            c.admin_claim = Some("groups".to_owned());
            c.admin_value = Some("/calcraft-admins".to_owned());
        },
        Duration::from_secs(60),
    )
    .await;

    // Same e-mail as the local admin: no linking by e-mail, a new account.
    let reply = login(&app, &idp, "carol", "sub-carol").await;
    assert_eq!(reply.location(), "/");
    let session = reply.cookie("calcraft_session").unwrap();
    let me = call(&app, Method::GET, "/api/v1/me", Some(&session), None).await;
    assert_eq!(me.json["username"], "carol");
    assert_eq!(me.json["display_name"], "Some User");
    assert_eq!(me.json["is_admin"], false);

    // SSO-created accounts have no usable password.
    for password in ["", "!sso-only", "x"] {
        let reply = call(
            &app,
            Method::POST,
            "/api/v1/auth/login",
            None,
            Some(json!({"username": "carol", "password": password})),
        )
        .await;
        assert_ne!(reply.status, StatusCode::OK);
    }

    // They can create app passwords for DAV clients.
    let created = call(
        &app,
        Method::POST,
        "/api/v1/app-passwords",
        Some(&session),
        Some(json!({"name": "phone"})),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);

    // Admin rights follow the group claim, in both directions.
    let (request, binding) = start(&app, &idp).await;
    let mut claims = idp.claims(&request, "carol", "sub-carol");
    claims["groups"] = json!(["/staff", "/calcraft-admins"]);
    let code = idp.authorize(&request, claims);
    let session = callback(&app, &request["state"], &code, Some(&binding))
        .await
        .cookie("calcraft_session")
        .unwrap();
    let me = call(&app, Method::GET, "/api/v1/me", Some(&session), None).await;
    assert_eq!(me.json["is_admin"], true);
    let session = login(&app, &idp, "carol", "sub-carol")
        .await
        .cookie("calcraft_session")
        .unwrap();
    let me = call(&app, Method::GET, "/api/v1/me", Some(&session), None).await;
    assert_eq!(me.json["is_admin"], false);
}

#[tokio::test]
async fn jwks_rotation_is_refetched_with_rate_limit() {
    // Unknown kid triggers a refetch.
    let idp = Idp::start().await;
    let fast = app_with(&idp, |_| {}, Duration::ZERO).await;
    assert_eq!(login(&fast, &idp, "alice", "s").await.location(), "/");
    idp.state.lock().unwrap().keys.push(Key::new("k2"));
    assert_eq!(login(&fast, &idp, "alice", "s").await.location(), "/");
    assert_eq!(idp.state.lock().unwrap().jwks_fetches, 2);

    // With the default limit, an unknown kid shortly after a fetch does not
    // cause another request.
    let idp = Idp::start().await;
    let app = app(&idp).await;
    assert_eq!(login(&app, &idp, "alice", "s").await.location(), "/");
    idp.state.lock().unwrap().keys = vec![Key::new("k3")];
    assert_failed(&login(&app, &idp, "alice", "s").await, "login_failed");
    assert_eq!(idp.state.lock().unwrap().jwks_fetches, 1);
}

#[tokio::test]
async fn sso_endpoints_are_absent_when_not_configured() {
    let store = Store::in_memory().await.unwrap();
    let app = router(&Config::default(), AppState::new(store).unwrap());
    let methods = call(&app, Method::GET, "/api/v1/auth/methods", None, None).await;
    assert_eq!(methods.json["oidc"], Value::Null);
    for uri in [
        "/api/v1/auth/oidc/login",
        "/api/v1/auth/oidc/callback?code=a&state=b",
    ] {
        assert_eq!(
            call(&app, Method::GET, uri, None, None).await.status,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn unreachable_idp_fails_cleanly() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let config = OidcConfig {
        enabled: true,
        issuer,
        client_id: CLIENT_ID.to_owned(),
        client_secret: Some(Secret::new(CLIENT_SECRET)),
        redirect_uri: REDIRECT_URI.to_owned(),
        allow_insecure_issuer_for_testing: true,
        ..OidcConfig::default()
    };
    let mut state = AppState::new(Store::in_memory().await.unwrap()).unwrap();
    state.oidc = Some(Oidc::new(&config).unwrap());
    let app = router(&Config::default(), state);
    let reply = call(&app, Method::GET, "/api/v1/auth/oidc/login", None, None).await;
    assert_failed(&reply, "unavailable");
}
