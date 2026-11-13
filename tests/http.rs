use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use saic_rust_notify::{
    controls::{climate_plan, parse_temperature, Controls, Publish, ResultMessage},
    http::router,
    telemetry::{decode, Telemetry},
};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, RwLock};
use tower::ServiceExt;
const BASE: &str = "saic/test/vehicles/CAR";
async fn setup() -> (Controls, mpsc::Receiver<Publish>) {
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
    for (t, count, first) in [
        (4., 5, "frontWindowDefrosterHeating"),
        (4.01, 4, "remoteClimateState"),
        (6., 4, "remoteClimateState"),
        (6.01, 1, "remoteClimateState"),
    ] {
        let (commands, _) = climate_plan(t);
        assert_eq!(commands.len(), count);
        assert_eq!(commands[0]["topic"], format!("climate/{first}/set"));
        if count > 1 {
            assert_eq!(
                commands[1],
                json!({"topic":"climate/heatedSeatsFrontLeftLevel/set","payload":"3"})
            );
            assert_eq!(
                commands[2],
                json!({"topic":"climate/heatedSeatsFrontRightLevel/set","payload":"3"})
            );
            assert_eq!(
                commands[3],
                json!({"topic":"climate/steeringWheelHeating/set","payload":"on"})
            );
        }
        if count == 5 {
            assert_eq!(
                commands[4]["topic"],
                "climate/rearWindowDefrosterHeating/set"
            );
        }
    }
}
fn deliver(c: &Controls, topic: &str, value: Value, retained: bool) {
    let _ = c.results.send(ResultMessage {
        topic: topic.into(),
        value,
        retained,
    });
}
fn success(payload: &Value) -> Value {
    json!({"requestId":payload["requestId"],"status":"success","results":payload["commands"].as_array().unwrap().iter().map(|c| json!({"topic":c["topic"],"status":"success"})).collect::<Vec<_>>()})
}
#[tokio::test]
async fn climate_success_requires_ack_and_exact_matching_result() {
    let (c, mut rx) = setup().await;
    let mut previous = Value::Null;
    for _ in 0..2 {
        c.preheating.send_replace(None);
        let task = tokio::spawn(call(
            router(c.clone(), Some("test-secret")),
            "POST",
            "/climate/start",
            r#"{"temperature":"35.6 °F"}"#,
            Some("Bearer test-secret"),
            None,
        ));
        let publish = rx.recv().await.unwrap();
        assert_eq!(publish.topic, format!("{BASE}/commands/batch/set"));
        let payload: Value = serde_json::from_str(&publish.payload).unwrap();
        assert_ne!(payload["requestId"], previous);
        previous = payload["requestId"].clone();
        assert_eq!(payload["commands"], json!(climate_plan(2.).0));
        let result = success(&payload);
        let topic = format!("{BASE}/commands/batch/result");
        deliver(&c, &topic, result.clone(), true);
        deliver(
            &c,
            &topic,
            json!({"requestId":"other","status":"success"}),
            false,
        );
        deliver(
            &c,
            "saic/test/vehicles/OTHER/commands/batch/result",
            result.clone(),
            false,
        );
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        assert!(c.preheating.borrow().is_none());
        deliver(&c, &topic, result, false);
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "result alone must not replace PUBACK");
        publish.ack.send(()).unwrap();
        assert_eq!(
            task.await.unwrap(),
            (200, "Heating up and defrosting".into())
        );
        assert!(c.preheating.borrow().is_some());
    }
}
#[tokio::test(start_paused = true)]
async fn failures_timeouts_and_mutual_exclusion() {
    for outcome in [
        "partial",
        "rejected",
        "unknown",
        "timeout",
        "disconnect",
        "ack-timeout",
    ] {
        let (c, mut rx) = setup().await;
        let task = tokio::spawn(call(
            router(c.clone(), Some("test-secret")),
            "POST",
            "/climate/start",
            "",
            Some("Bearer test-secret"),
            None,
        ));
        let publish = rx.recv().await.unwrap();
        assert!(c
            .execute("/locate", None)
            .await
            .unwrap_err()
            .contains("in progress"));
        assert!(c
            .execute("/climate/start", Some(2.))
            .await
            .unwrap_err()
            .contains("in progress"));
        let payload: Value = serde_json::from_str(&publish.payload).unwrap();
        if outcome != "ack-timeout" {
            publish.ack.send(()).unwrap();
        } else {
            tokio::time::advance(Duration::from_secs(6)).await;
        }
        let mut result = success(&payload);
        match outcome {
            "partial" => result["results"][1]["status"] = json!("failed"),
            "rejected" => result["results"] = json!([]),
            "unknown" => result["results"][0]["status"] = json!("unknown"),
            "disconnect" => c.state.write().await.set_connected(false),
            _ => {}
        }
        if matches!(outcome, "partial" | "rejected" | "unknown") {
            deliver(&c, &format!("{BASE}/commands/batch/result"), result, false);
        }
        let (status, body) = task.await.unwrap();
        assert_eq!(status, 503);
        assert!(body.contains("earlier commands may have succeeded"));
        assert!(
            body.contains(
                if matches!(outcome, "timeout" | "disconnect" | "ack-timeout") {
                    "unknown"
                } else {
                    outcome
                }
            ),
            "{body}"
        );
        assert!(c.preheating.borrow().is_none());
        assert!(rx.try_recv().is_err());
    }
}
#[tokio::test(start_paused = true)]
async fn refresh_and_lights_only_locate() {
    for (path, suffix, payload, code) in [
        ("/refresh", "refresh/mode/set", "force", 202),
        ("/locate", "location/findMyCar/set", "lights_only", 200),
    ] {
        for outcome in ["success", "rejected", "timeout", "ack-timeout"] {
            let (c, mut rx) = setup().await;
            let task = tokio::spawn(call(
                router(c.clone(), Some("test-secret")),
                "POST",
                path,
                "",
                Some("Bearer test-secret"),
                None,
            ));
            let p = rx.recv().await.unwrap();
            assert_eq!(p.topic, format!("{BASE}/{suffix}"));
            assert_eq!(p.payload, payload);
            if path == "/locate" {
                assert!(c.execute("/climate/start", Some(2.)).await.is_err());
            }
            if outcome == "ack-timeout" {
                tokio::time::advance(Duration::from_secs(6)).await;
            } else {
                p.ack.send(()).unwrap();
            }
            if path == "/locate" && matches!(outcome, "success" | "rejected") {
                deliver(
                    &c,
                    &format!("{BASE}/location/findMyCar/result"),
                    json!("Success"),
                    true,
                );
                deliver(
                    &c,
                    &format!("{BASE}/location/findMyCar/result"),
                    json!(if outcome == "success" {
                        "Success"
                    } else {
                        "Failed"
                    }),
                    false,
                );
            }
            let (status, _) = task.await.unwrap();
            assert_eq!(
                status,
                if outcome == "ack-timeout" || (path == "/locate" && outcome != "success") {
                    503
                } else {
                    code
                }
            );
            assert!(c.preheating.borrow().is_none());
        }
    }
}
#[tokio::test]
async fn status_freshness_and_temperature_selection() {
    let (c, mut rx) = setup().await;
    let now = Instant::now();
    {
        let mut s = c.state.write().await;
        s.ingest(
            decode(&format!("{BASE}/climate/exteriorTemperature"), b"2", None)
                .unwrap()
                .unwrap(),
            false,
            now,
        )
        .unwrap();
    }
    let (code, body) = call(
        router(c.clone(), Some("test-secret")),
        "GET",
        "/api/status",
        "",
        Some("Bearer test-secret"),
        None,
    )
    .await;
    assert_eq!(code, 200);
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["connected"], true);
    assert_eq!(
        body["readings"]["vehicle/climate/exteriorTemperature"],
        json!({"value":"2","retained":false,"fresh":true})
    );
    let cc = c.clone();
    let task = tokio::spawn(async move { cc.execute("/climate/start", None).await });
    let p = rx.recv().await.unwrap();
    let value: Value = serde_json::from_str(&p.payload).unwrap();
    assert_eq!(value["commands"], json!(climate_plan(2.).0));
    p.ack.send(()).unwrap();
    deliver(
        &c,
        &format!("{BASE}/commands/batch/result"),
        success(&value),
        false,
    );
    task.await.unwrap().unwrap();
    // Both cached retained temperatures are unusable without a fresh gateway timestamp.
    for side in ["interior", "exterior"] {
        c.state
            .write()
            .await
            .ingest(
                decode(&format!("{BASE}/climate/{side}Temperature"), b"-3", None)
                    .unwrap()
                    .unwrap(),
                true,
                now,
            )
            .unwrap();
    }
    assert!(c
        .execute("/climate/start", None)
        .await
        .unwrap_err()
        .contains("temperature"));
    c.state.write().await.set_connected(false);
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
    for path in ["/refresh", "/locate", "/climate/start"] {
        assert!(c.execute(path, Some(2.)).await.is_err());
    }
}

#[tokio::test]
async fn fresh_temperature_excludes_stale_lower_value_and_status_uses_injected_clock() {
    let (c, mut rx) = setup().await;
    let now = Instant::now();
    c.state
        .write()
        .await
        .ingest(
            decode(&format!("{BASE}/climate/exteriorTemperature"), b"-20", None)
                .unwrap()
                .unwrap(),
            false,
            now - Duration::from_secs(601),
        )
        .unwrap();
    let snapshot = c
        .state
        .read()
        .await
        .snapshot(now, chrono::Utc::now(), c.ttl);
    assert_eq!(
        snapshot["readings"]["vehicle/climate/interiorTemperature"]["fresh"],
        true
    );
    assert_eq!(
        snapshot["readings"]["vehicle/climate/exteriorTemperature"]["fresh"],
        false
    );
    let cc = c.clone();
    let task = tokio::spawn(async move { cc.execute("/climate/start", None).await });
    let p = rx.recv().await.unwrap();
    let payload: Value = serde_json::from_str(&p.payload).unwrap();
    assert_eq!(payload["commands"], json!(climate_plan(5.).0));
    p.ack.send(()).unwrap();
    deliver(
        &c,
        &format!("{BASE}/commands/batch/result"),
        success(&payload),
        false,
    );
    task.await.unwrap().unwrap();
    let snapshot =
        c.state
            .read()
            .await
            .snapshot(now + Duration::from_secs(601), chrono::Utc::now(), c.ttl);
    assert_eq!(
        snapshot["readings"]["vehicle/climate/interiorTemperature"]["fresh"],
        false
    );
}

#[tokio::test]
async fn status_preserves_unique_keys_and_isolates_multiple_vehicles() {
    let (c, _) = setup().await;
    let mut state = c.state.write().await;
    let now = Instant::now();
    state
        .ingest(
            decode("saic/test/status", b"online", None)
                .unwrap()
                .unwrap(),
            false,
            now,
        )
        .unwrap();
    let snapshot = state.snapshot(now, chrono::Utc::now(), c.ttl);
    assert_eq!(snapshot["readings"]["account/status"]["value"], "online");
    assert_eq!(
        snapshot["readings"]["vehicle/climate/interiorTemperature"]["value"],
        "5"
    );
    state
        .ingest(
            decode(
                "saic/test/vehicles/OTHER/climate/interiorTemperature",
                b"20",
                None,
            )
            .unwrap()
            .unwrap(),
            true,
            now,
        )
        .unwrap();
    let snapshot = state.snapshot(now, chrono::Utc::now(), c.ttl);
    assert_eq!(
        snapshot["readings"]["saic/test/vehicles/CAR/vehicle/climate/interiorTemperature"]["value"],
        "5"
    );
    assert_eq!(
        snapshot["readings"]["saic/test/vehicles/OTHER/vehicle/climate/interiorTemperature"]
            ["value"],
        "20"
    );
    drop(state);
    assert!(c.execute("/refresh", None).await.is_err());
}
