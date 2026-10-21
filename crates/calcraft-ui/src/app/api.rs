//! Calls to the REST API (`/api/v1/…`).
//!
//! The session is an `HttpOnly` cookie that this code never sees; fetches
//! use `credentials: same-origin`. Every state-changing request carries the
//! CSRF header.

use calcraft_core::api::CSRF_HEADER;
use calcraft_ui::ApiFailure;
use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestCredentials, RequestInit, Response};

const API_BASE: &str = "/api";

fn js_error(value: JsValue) -> ApiFailure {
    ApiFailure::network(value.as_string().unwrap_or_default())
}

/// Sends a request and returns status and body text.
async fn call(method: &str, path: &str, body: Option<String>) -> Result<(u16, String), ApiFailure> {
    let window = web_sys::window().ok_or_else(|| ApiFailure::network("no window"))?;
    let init = RequestInit::new();
    init.set_method(method);
    init.set_credentials(RequestCredentials::SameOrigin);
    let headers = Headers::new().map_err(js_error)?;
    headers
        .set("accept", "application/json")
        .map_err(js_error)?;
    if method != "GET" {
        headers.set(CSRF_HEADER, "1").map_err(js_error)?;
    }
    if let Some(body) = body {
        headers
            .set("content-type", "application/json")
            .map_err(js_error)?;
        init.set_body(&JsValue::from_str(&body));
    }
    init.set_headers(&headers);
    let request =
        Request::new_with_str_and_init(&format!("{API_BASE}{path}"), &init).map_err(js_error)?;
    let response: Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(js_error)?
        .dyn_into()
        .map_err(js_error)?;
    let text = JsFuture::from(response.text().map_err(js_error)?)
        .await
        .map_err(js_error)?
        .as_string()
        .unwrap_or_default();
    Ok((response.status(), text))
}

fn encode<B: Serialize>(body: &B) -> Result<String, ApiFailure> {
    serde_json::to_string(body).map_err(|err| ApiFailure::unexpected(err.to_string()))
}

fn decode<T: DeserializeOwned>(status: u16, text: &str) -> Result<T, ApiFailure> {
    if (200..300).contains(&status) {
        serde_json::from_str(text).map_err(|err| ApiFailure::unexpected(err.to_string()))
    } else {
        Err(ApiFailure::from_response(status, text))
    }
}

fn expect_empty(status: u16, text: &str) -> Result<(), ApiFailure> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(ApiFailure::from_response(status, text))
    }
}

/// `GET` with a JSON response.
pub async fn get<T: DeserializeOwned>(path: &str) -> Result<T, ApiFailure> {
    let (status, text) = call("GET", path, None).await?;
    decode(status, &text)
}

/// Request with a JSON body and a JSON response.
pub async fn send<B: Serialize, T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: &B,
) -> Result<T, ApiFailure> {
    let (status, text) = call(method, path, Some(encode(body)?)).await?;
    decode(status, &text)
}

/// Request with a JSON body and no response body (`204`).
pub async fn send_no_content<B: Serialize>(
    method: &str,
    path: &str,
    body: &B,
) -> Result<(), ApiFailure> {
    let (status, text) = call(method, path, Some(encode(body)?)).await?;
    expect_empty(status, &text)
}

/// Request without body and without response body (`DELETE`, logout).
pub async fn send_empty(method: &str, path: &str) -> Result<(), ApiFailure> {
    let (status, text) = call(method, path, None).await?;
    expect_empty(status, &text)
}

/// `POST` without body with a JSON response (2FA setup).
pub async fn post_for<T: DeserializeOwned>(path: &str) -> Result<T, ApiFailure> {
    let (status, text) = call("POST", path, None).await?;
    decode(status, &text)
}
