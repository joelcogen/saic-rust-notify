use crate::controls::{parse_temperature, Controls};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

#[derive(Clone)]
struct Api {
    controls: Controls,
    token: Option<[u8; 32]>,
}
pub fn router(controls: Controls, token: Option<&str>) -> Router {
    let token = token
        .filter(|t| !t.is_empty())
        .map(|t| Sha256::digest(format!("Bearer {t}").as_bytes()).into());
    Router::new()
        .fallback(handle)
        .with_state(Api { controls, token })
}
async fn handle(State(api): State<Api>, request: Request) -> Response {
    // The public shell contains no vehicle data. API and control routes still
    // require the bearer token, supplied by the browser after unlocking.
    let asset = if request.method() == "GET" || request.method() == "HEAD" {
        match request.uri().path() {
            "/" | "/dashboard.html" => Some((
                "text/html; charset=utf-8",
                include_bytes!("dashboard/index.html").as_slice(),
            )),
            "/tabler-1.7.5.min.css" => Some((
                "text/css; charset=utf-8",
                include_bytes!("dashboard/vendor/tabler-1.7.5.min.css").as_slice(),
            )),
            "/dashboard.css" => Some((
                "text/css; charset=utf-8",
                include_bytes!("dashboard/dashboard.css").as_slice(),
            )),
            "/dashboard.js" => Some((
                "text/javascript; charset=utf-8",
                include_bytes!("dashboard/dashboard.js").as_slice(),
            )),
            "/dashboard-model.mjs" => Some((
                "text/javascript; charset=utf-8",
                include_bytes!("dashboard/dashboard-model.mjs").as_slice(),
            )),
            "/car.jpg" => Some(("image/jpeg", include_bytes!("dashboard/car.jpg").as_slice())),
            "/favicon.svg" => Some((
                "image/svg+xml",
                include_bytes!("dashboard/favicon.svg").as_slice(),
            )),
            _ => None,
        }
    } else {
        None
    };
    let mut response = if let Some((mime, bytes)) = asset {
        let body = if request.method() == "HEAD" {
            &[][..]
        } else {
            bytes
        };
        ([(header::CONTENT_TYPE, mime)], body).into_response()
    } else {
        respond(api, request).await
    };
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    ] {
        response.headers_mut().insert(name, value.parse().unwrap());
    }
    response
}
async fn respond(api: Api, request: Request) -> Response {
    let unauthorized = || (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    let Some(expected) = api.token else {
        return unauthorized();
    };
    let mut auth = request.headers().get_all(header::AUTHORIZATION).iter();
    let supplied: [u8; 32] = Sha256::digest(auth.next().map_or(&[][..], |v| v.as_bytes())).into();
    if !bool::from(expected.ct_eq(&supplied)) || auth.next().is_some() {
        return unauthorized();
    }
    let path = request.uri().path().to_owned();
    let status = request.method() == "GET" && path == "/api/status";
    if !(status
        || request.method() == "POST"
            && matches!(
                path.as_str(),
                "/climate/start" | "/refresh" | "/locate" | "/battery/heat"
            ))
    {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    if request.headers().contains_key(header::TRANSFER_ENCODING) {
        return (
            StatusCode::BAD_REQUEST,
            "Transfer-Encoding is not supported",
        )
            .into_response();
    }
    let length = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .map(|v| v.to_str().ok().and_then(|s| s.parse::<usize>().ok()));
    if length.is_some_and(|n| n.is_none_or(|n| n > 4096)) {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid Content-Length; maximum is 4096 bytes",
        )
            .into_response();
    }
    let body =
        match tokio::time::timeout(Duration::from_secs(5), to_bytes(request.into_body(), 4096))
            .await
        {
            Ok(Ok(body)) => body,
            _ => {
                return (StatusCode::BAD_REQUEST, "Invalid or oversized request body")
                    .into_response()
            }
        };
    if length.is_some_and(|n| n != Some(body.len()))
        || (!body.is_empty() && path != "/climate/start")
    {
        return (StatusCode::BAD_REQUEST, "Unexpected request body").into_response();
    }
    let mut temperature = None;
    if !body.is_empty() {
        let value = match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v)
                if v.as_object()
                    .is_some_and(|o| o.keys().all(|k| k == "temperature")) =>
            {
                v
            }
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    "Expected JSON object with optional temperature only",
                )
                    .into_response()
            }
        };
        if let Some(v) = value.get("temperature") {
            match parse_temperature(v) {
                Ok(t) => temperature = Some(t),
                Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
            }
        }
    }
    if status {
        return axum::Json(api.controls.state.read().await.snapshot(
            Instant::now(),
            chrono::Utc::now(),
            api.controls.ttl,
        ))
        .into_response();
    }
    match api.controls.execute(&path, temperature).await {
        Ok(message) => (
            if path == "/refresh" {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            message,
        )
            .into_response(),
        Err(message) => (StatusCode::SERVICE_UNAVAILABLE, message).into_response(),
    }
}
