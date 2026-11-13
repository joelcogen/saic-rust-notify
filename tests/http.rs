use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use saic_rust_notify::{
    controls::{climate_plan, parse_temperature, Controls},
    http::router,
    telemetry::{decode, Telemetry},
    vehicle::{Action, Request as VehicleRequest},
};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, RwLock};
use tower::ServiceExt;
const BASE: &str = "saic/test/vehicles/CAR";
async fn setup() -> (Controls, mpsc::Receiver<VehicleRequest>) {
    let state = Arc::new(RwLock::new(Telemetry::default()));
    state.write().await.set_connected(true);
    state
        .write()
        .await
        .ingest(
            decode(&format!("{BASE}/climate/interiorTemperature"), b"5", None)
                .unwrap()
                .unwrap(),
            false,
            Instant::now(),
        )
        .unwrap();
    Controls::new(state, Duration::from_secs(600))
}
async fn call(
    app: Router,
    method: &str,
    path: &str,
    body: &str,
    token: Option<&str>,
    extra: Option<(&str, &str)>,
) -> (u16, String) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", token);
    }
    if let Some((key, value)) = extra {
        request = request.header(key, value);
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    assert!(response.headers().contains_key("content-security-policy"));
    (
        response.status().as_u16(),
        String::from_utf8(
            to_bytes(response.into_body(), 100000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap(),
    )
}
#[tokio::test]
async fn auth_validation_and_no_dashboard() {
    let (controls, mut rx) = setup().await;
    let app = router(controls.clone(), Some("test-secret"));
    for path in ["/api/status", "/climate/start", "/refresh", "/locate"] {
        for token in [None, Some("Bearer wrong"), Some("bearer test-secret")] {
            assert_eq!(
                call(app.clone(), "POST", path, "", token, None).await.0,
                401
            );
        }
        assert_eq!(
            call(
                router(controls.clone(), None),
                "POST",
                path,
                "",
                Some("Bearer test-secret"),
                None
            )
            .await
            .0,
            401
        );
    }
    for path in [
        "/",
        "/dashboard.html",
        "/dashboard.css",
        "/dashboard.js",
        "/missing",
    ] {
        assert_eq!(
            call(
                app.clone(),
                "GET",
                path,
                "",
                Some("Bearer test-secret"),
                None
            )
            .await
            .0,
            404
        );
    }
    for body in [
        "{",
        "[]",
        "null",
        r#"{"unexpected":3}"#,
        r#"{"temperature":true}"#,
        r#"{"temperature":null}"#,
        r#"{"temperature":"NaN"}"#,
        r#"{"temperature":1e999}"#,
    ] {
        assert_eq!(
            call(
                app.clone(),
                "POST",
                "/climate/start",
                body,
                Some("Bearer test-secret"),
                None
            )
            .await
            .0,
            400,
            "{body}"
        );
    }
    assert_eq!(
        call(
            app.clone(),
            "POST",
            "/climate/start",
            &" ".repeat(4097),
            Some("Bearer test-secret"),
            None
        )
        .await
        .0,
        400
    );
    for path in ["/api/status", "/refresh", "/locate"] {
        assert_eq!(
            call(
                app.clone(),
                if path == "/api/status" { "GET" } else { "POST" },
                path,
                "{}",
                Some("Bearer test-secret"),
                None
            )
            .await
            .0,
            400
        );
    }
    for header in [
        ("transfer-encoding", "chunked"),
        ("content-length", "-1"),
        ("content-length", "4097"),
        ("content-length", "wat"),
        ("content-length", "2"),
    ] {
        assert_eq!(
            call(
                app.clone(),
                "POST",
                "/climate/start",
                "",
                Some("Bearer test-secret"),
                Some(header)
            )
            .await
            .0,
            400
        );
    }
    assert!(rx.try_recv().is_err());
}
#[test]
fn temperatures_and_thresholds() {
    for (raw, expected) in [
        (json!(32), 32.),
        (json!("32 °F"), 0.),
        (json!(" 42,8 º f "), 6.),
        (json!("+.5°C"), 0.5),
        (json!("-4"), -4.),
    ] {
        assert!((parse_temperature(&raw).unwrap() - expected).abs() < 1e-8);
    }
    for raw in [
        json!(true),
        json!(null),
        json!([]),
        json!("infinity"),
        json!("1e2"),
        json!("6 kelvin"),
        json!("1 2 C"),
    ] {
        assert!(parse_temperature(&raw).is_err());
    }
    for (t, count) in [(4., 5), (4.01, 4), (6., 4), (6.01, 1)] {
        assert_eq!(climate_plan(t).0.len(), count);
    }
}

#[tokio::test]
async fn controls_wait_for_completion_and_notify_only_on_full_success() {
    let (c, mut rx) = setup().await;
    for outcome in [
        Ok("Heating up"),
        Err("Climate partial failure"),
        Err(saic_rust_notify::controls::UNKNOWN),
    ] {
        c.preheating.send_replace(None);
        let caller = c.clone();
        let task = tokio::spawn(async move { caller.execute("/climate/start", None).await });
        let request = rx.recv().await.unwrap();
        assert_eq!(request.vin, BASE);
        assert!(matches!(request.action, Action::Climate(5.)));
        assert!(c.preheating.borrow().is_none());
        assert!(!task.is_finished());
        assert!(c
            .execute("/locate", None)
            .await
            .unwrap_err()
            .contains("in progress"));
        request.reply.send(outcome).unwrap();
        assert_eq!(task.await.unwrap(), outcome);
        assert_eq!(c.preheating.borrow().is_some(), outcome.is_ok());
        assert!(rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn refresh_locate_and_temperature_selection() {
    let (c, mut rx) = setup().await;
    for (path, expected) in [("/refresh", 202), ("/locate", 200), ("/climate/start", 200)] {
        let task = tokio::spawn(call(
            router(c.clone(), Some("test-secret")),
            "POST",
            path,
            "",
            Some("Bearer test-secret"),
            None,
        ));
        let request = rx.recv().await.unwrap();
        assert!(matches!(
            (path, request.action),
            ("/refresh", Action::Refresh)
                | ("/locate", Action::Locate)
                | ("/climate/start", Action::Climate(5.))
        ));
        request.reply.send(Ok("done")).unwrap();
        assert_eq!(task.await.unwrap().0, expected);
    }
    c.state
        .write()
        .await
        .ingest(
            decode(&format!("{BASE}/climate/exteriorTemperature"), b"-20", None)
                .unwrap()
                .unwrap(),
            false,
            Instant::now() - Duration::from_secs(601),
        )
        .unwrap();
    let caller = c.clone();
    let task = tokio::spawn(async move { caller.execute("/climate/start", None).await });
    let request = rx.recv().await.unwrap();
    assert!(matches!(request.action, Action::Climate(5.)));
    request.reply.send(Ok("done")).unwrap();
    task.await.unwrap().unwrap();
    c.state.write().await.set_connected(false);
    for path in ["/refresh", "/locate", "/climate/start"] {
        assert!(c.execute(path, Some(2.)).await.is_err());
    }
    assert!(rx.try_recv().is_err());
    let (_, body) = call(
        router(c.clone(), Some("test-secret")),
        "GET",
        "/api/status",
        "",
        Some("Bearer test-secret"),
        None,
    )
    .await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["connected"], false);
    assert_eq!(
        body["readings"]["vehicle/climate/interiorTemperature"]["fresh"],
        false
    );
}

#[tokio::test(start_paused = true)]
async fn timed_out_request_is_cancelled_and_ambiguous_vehicles_refuse_controls() {
    let (c, mut rx) = setup().await;
    let caller = c.clone();
    let task = tokio::spawn(async move { caller.execute("/locate", None).await });
    let request = rx.recv().await.unwrap();
    tokio::time::advance(Duration::from_secs(361)).await;
    assert!(task.await.unwrap().unwrap_err().contains("unknown"));
    assert!(request.reply.is_closed());
    c.state
        .write()
        .await
        .select_vehicles(&[BASE.into(), "OTHER".into()]);
    assert!(c.execute("/locate", None).await.is_err());
    assert!(rx.try_recv().is_err());
}
