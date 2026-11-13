use super::*;
use crate::{
    controls::{Controls, ResultMessage},
    telemetry::{decode, Telemetry},
};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::StatusCode,
    response::Response,
    Router,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::RwLock;

type Call = (Method, String, Value);
struct Reply(u16, Value, Option<&'static str>);
struct Mock {
    replies: Mutex<VecDeque<Reply>>,
    calls: Mutex<Vec<Call>>,
}
struct Server {
    mock: Arc<Mock>,
    task: tokio::task::JoinHandle<()>,
    url: Url,
    api: Api,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new(replies: Vec<Reply>) -> Self {
        async fn handle(State(mock): State<Arc<Mock>>, request: Request) -> Response {
            let method = request.method().clone();
            let uri = request.uri().to_string();
            let bytes = to_bytes(request.into_body(), 256 * 1024).await.unwrap();
            let body = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            };
            mock.calls.lock().unwrap().push((method, uri, body));
            let Reply(status, body, retry) = mock
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected Notify request");
            let mut response = Response::builder().status(StatusCode::from_u16(status).unwrap());
            if let Some(retry) = retry {
                response = response.header("retry-after", retry);
            }
            response.body(Body::from(body.to_string())).unwrap()
        }
        let mock = Arc::new(Mock {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(vec![]),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/activities/device?token=synthetic",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let app = Router::new().fallback(handle).with_state(mock.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            mock,
            task,
            url,
            api: Api(Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap()),
        }
    }
    fn calls(&self) -> Vec<Call> {
        self.mock.calls.lock().unwrap().clone()
    }
    fn done(&self) {
        assert!(self.mock.replies.lock().unwrap().is_empty());
    }
}
fn ok(value: Value) -> Reply {
    Reply(200, value, None)
}
fn empty() -> Reply {
    ok(json!({"activities":[]}))
}
fn readings(values: &[(&str, &str)]) -> Readings {
    let latest: BTreeMap<_, _> = values
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();
    Readings {
        fresh: latest.clone(),
        latest,
    }
}
fn active() -> Readings {
    readings(&[
        ("drivetrain/charging", "true"),
        ("drivetrain/soc", "52"),
        ("drivetrain/socTarget", "80"),
        ("drivetrain/power", "-6.5"),
        ("drivetrain/remainingChargingTime", "900"),
    ])
}

#[test]
fn payload_contracts_and_url_validation() {
    let mut r = readings(&[
        ("drivetrain/soc", "52"),
        ("doors/locked", "true"),
        ("drivetrain/charging", "true"),
        ("climate/interiorTemperature", "5"),
        ("climate/exteriorTemperature", "-2"),
        ("climate/remoteClimateState", "on"),
        ("climate/heatedSeatsFrontRightLevel", "3"),
        ("climate/rearWindowDefrosterHeating", "true"),
    ]);
    assert_eq!(
        dashboard(&r),
        json!({"trailing":"52%","metrics":[
        {"color":"#22C55E","label":"Doors","value":"Locked"},
        {"color":"#22C55E","label":"Status","value":"Charging"},
        {"color":"#22C55E","label":"Temperature","value":"5º • -2º"},
        {"color":"#22C55E","label":"A/C","value":"On"}],"status":"Current"})
    );
    assert_eq!(
        climate(&r),
        json!({"title":PREHEATING,"body":"5º • Seat warmers • Defrost","symbol":"flame.fill","tint":"#EF4444","steps":20,"step":4})
    );
    assert_eq!(
        charging_payload(&active()),
        json!({"title":CHARGING,"symbol":"bolt.car.fill","tint":"#EF4444","body":"6.5 kW • 52%","progress":52.,"status":"charging","endsIn":900})
    );
    r.fresh.clear();
    assert_eq!(dashboard(&r)["status"], "Last known / stale");
    assert_eq!(charging_payload(&r)["progress"], Value::Null);
    assert_eq!(climate(&Readings::default())["body"], "?");
    for invalid in [
        "file:///secret",
        "https://user:secret@example.com",
        "https://example.com/#secret",
    ] {
        let error = Config::load(|_| Ok(Some(invalid.into()))).err().unwrap();
        assert!(!error.contains("secret"));
    }
    let url = Url::parse("https://example.com/activities/device?token=synthetic").unwrap();
    assert_eq!(
        activity_url(&url, &json!({"activityId":"a/b ?"}))
            .unwrap()
            .as_str(),
        "https://example.com/activities/a%2Fb%20%3F?token=synthetic"
    );
}

#[tokio::test]
async fn dashboard_deduplicates_refreshes_and_backs_off() {
    let s = Server::new(vec![
        ok(json!({})),
        ok(json!({})),
        Reply(429, json!({}), Some("120")),
        ok(json!({})),
    ])
    .await;
    // Use a future monotonic origin so HTTP elapsed time cannot advance the injected clock.
    let now = Instant::now() + Duration::from_secs(3600);
    let mut worker = Dashboard::new(now);
    let mut r = active();
    worker.tick(&s.api, &s.url, &r, now).await;
    worker
        .tick(&s.api, &s.url, &r, now + Duration::from_secs(299))
        .await;
    assert_eq!(s.calls().len(), 1);
    worker
        .tick(&s.api, &s.url, &r, now + Duration::from_secs(304))
        .await;
    r.latest.insert("drivetrain/soc".into(), json!(53));
    worker
        .tick(&s.api, &s.url, &r, now + Duration::from_secs(309))
        .await;
    worker
        .tick(&s.api, &s.url, &r, now + Duration::from_secs(428))
        .await;
    assert_eq!(s.calls().len(), 3);
    worker
        .tick(&s.api, &s.url, &r, now + Duration::from_secs(429))
        .await;
    s.done();
}

#[tokio::test]
async fn uncertain_creation_recovers_without_duplicate_and_completes_charging() {
    let s = Server::new(vec![
        empty(),
        Reply(500, json!({}), None),
        ok(json!({"activities":[{"activityId":"existing"}]})),
        ok(json!({"activityId":"existing","content":{"title":CHARGING}})),
        ok(json!({})),
        ok(json!({})),
        ok(json!({"activityId":"second"})),
        ok(json!({})),
    ])
    .await;
    // Use a future monotonic origin so HTTP elapsed time cannot advance the injected clock.
    let now = Instant::now() + Duration::from_secs(3600);
    let wall = Utc::now();
    let mut w = Activity::new(false, now);
    let mut r = active();
    w.tick(&s.api, &s.url, &r, now, wall).await;
    assert!(!w.recovered);
    w.tick(&s.api, &s.url, &r, now + Duration::from_secs(29), wall)
        .await;
    assert_eq!(s.calls().len(), 2);
    w.tick(&s.api, &s.url, &r, now + Duration::from_secs(30), wall)
        .await;
    assert!(w.target.as_ref().unwrap().path().ends_with("existing"));
    r.fresh.insert("drivetrain/soc".into(), json!(80));
    w.tick(&s.api, &s.url, &r, now + Duration::from_secs(60), wall)
        .await;
    assert!(w.suppressed);
    r.fresh.insert("drivetrain/soc".into(), json!(79));
    w.tick(&s.api, &s.url, &r, now + Duration::from_secs(90), wall)
        .await;
    assert_eq!(s.calls().len(), 6);
    r.fresh.insert("drivetrain/charging".into(), json!(false));
    w.tick(&s.api, &s.url, &r, now + Duration::from_secs(120), wall)
        .await;
    assert!(!w.suppressed);
    r.fresh.insert("drivetrain/charging".into(), json!(true));
    w.tick(&s.api, &s.url, &r, now + Duration::from_secs(150), wall)
        .await;
    r.fresh.insert("drivetrain/charging".into(), json!(false));
    w.tick(&s.api, &s.url, &r, now + Duration::from_secs(180), wall)
        .await;
    let calls = s.calls();
    assert_eq!(calls[5].0, Method::DELETE);
    assert_eq!(calls[5].2["status"], "done");
    assert_eq!(
        calls[7].2,
        json!({"status":"stopped","body":"Charging stopped"})
    );
    assert_eq!(calls.iter().filter(|c| c.1.contains("new=1")).count(), 2);
    s.done();
}

#[tokio::test]
async fn discovery_rejects_ambiguity_and_stale_charging_never_finishes() {
    let s=Server::new(vec![ok(json!({"activities":[{"activityId":"one","content":{"title":CHARGING}},{"activityId":"two","content":{"title":CHARGING}}]})),ok(json!({"activities":[{"activityId":"one","content":{"title":CHARGING}}]}))]).await;
    // Use a future monotonic origin so HTTP elapsed time cannot advance the injected clock.
    let now = Instant::now() + Duration::from_secs(3600);
    let wall = Utc::now();
    let mut w = Activity::new(false, now);
    let mut r = active();
    r.fresh.clear();
    w.tick(&s.api, &s.url, &r, now, wall).await;
    assert!(!w.recovered);
    w.tick(&s.api, &s.url, &r, now + Duration::from_secs(30), wall)
        .await;
    assert!(w.target.is_some());
    w.tick(&s.api, &s.url, &r, now + Duration::from_secs(60), wall)
        .await;
    assert_eq!(s.calls().len(), 2);
    s.done();
}

#[tokio::test]
async fn gone_suppresses_and_rate_limit_is_bounded() {
    let s = Server::new(vec![
        empty(),
        Reply(429, json!({}), Some("99999999")),
        empty(),
        Reply(410, json!({}), None),
    ])
    .await;
    // Use a future monotonic origin so HTTP elapsed time cannot advance the injected clock.
    let now = Instant::now() + Duration::from_secs(3600);
    let wall = Utc::now();
    let mut w = Activity::new(false, now);
    w.tick(&s.api, &s.url, &active(), now, wall).await;
    assert_eq!(w.next, now + Duration::from_secs(3600));
    w.tick(
        &s.api,
        &s.url,
        &active(),
        now + Duration::from_secs(3599),
        wall,
    )
    .await;
    assert_eq!(s.calls().len(), 2);
    w.tick(
        &s.api,
        &s.url,
        &active(),
        now + Duration::from_secs(3600),
        wall,
    )
    .await;
    assert!(w.suppressed);
    assert!(w.target.is_none());
    w.tick(
        &s.api,
        &s.url,
        &active(),
        now + Duration::from_secs(3630),
        wall,
    )
    .await;
    assert_eq!(s.calls().len(), 4);
    s.done();
    for (header, expected) in [(Some("invalid"), 60), (Some("1"), 30), (None, 60)] {
        let s = Server::new(vec![Reply(429, json!({}), header)]).await;
        assert_eq!(
            s.api
                .request(&s.url, Method::GET, None)
                .await
                .err()
                .unwrap()
                .delay,
            Duration::from_secs(expected)
        );
    }
}

#[tokio::test]
async fn preheat_recovers_uncertain_creation_and_finishes_at_ten_minutes() {
    let s = Server::new(vec![
        empty(),
        Reply(502, json!({}), None),
        ok(json!({"activities":[{"activityId":"heat","content":{"title":PREHEATING}}]})),
        ok(json!({})),
        ok(json!({})),
    ])
    .await;
    // Use a future monotonic origin so HTTP elapsed time cannot advance the injected clock.
    let now = Instant::now() + Duration::from_secs(3600);
    let wall = Utc::now();
    let mut w = Activity::new(true, now);
    w.start(now);
    w.tick(&s.api, &s.url, &Readings::default(), now, wall)
        .await;
    w.tick(
        &s.api,
        &s.url,
        &Readings::default(),
        now + Duration::from_secs(30),
        wall,
    )
    .await;
    assert_eq!(s.calls()[3].2["endsIn"], 570);
    w.tick(
        &s.api,
        &s.url,
        &Readings::default(),
        now + Duration::from_secs(600),
        wall,
    )
    .await;
    assert!(w.deadline.is_none());
    assert!(w.target.is_none());
    assert_eq!(s.calls()[4].2, json!({"status":"done","keepFor":0}));
    assert_eq!(
        s.calls().iter().filter(|c| c.1.contains("new=1")).count(),
        1
    );
    s.done();
}

#[tokio::test]
async fn preheat_restart_uses_remote_deadline_and_gone_clears_it() {
    // Use a future monotonic origin so HTTP elapsed time cannot advance the injected clock.
    let now = Instant::now() + Duration::from_secs(3600);
    let wall = Utc::now();
    for end in [
        json!({"activityId":"heat","content":{"title":PREHEATING,"endsAt":wall.timestamp()+120}}),
        json!({"activityId":"heat","content":{"title":PREHEATING},"endsAt":(wall+chrono::Duration::seconds(120)).to_rfc3339()}),
    ] {
        let s = Server::new(vec![
            ok(json!({"activities":[end]})),
            Reply(410, json!({}), None),
        ])
        .await;
        let mut w = Activity::new(true, now);
        w.tick(&s.api, &s.url, &Readings::default(), now, wall)
            .await;
        assert_eq!(s.calls()[1].2["endsIn"], 120);
        assert!(w.deadline.is_none());
        w.tick(
            &s.api,
            &s.url,
            &Readings::default(),
            now + Duration::from_secs(60),
            wall,
        )
        .await;
        assert_eq!(s.calls().len(), 2);
        s.done();
    }
}

#[tokio::test]
async fn disconnected_and_ambiguous_telemetry_is_not_fresh() {
    let state = Arc::new(RwLock::new(Telemetry::default()));
    // Use a future monotonic origin so HTTP elapsed time cannot advance the injected clock.
    let now = Instant::now() + Duration::from_secs(3600);
    let wall = Utc::now();
    let ttl = Duration::from_secs(600);
    state
        .write()
        .await
        .ingest(
            decode("saic/test/vehicles/CAR/drivetrain/soc", b"52", None)
                .unwrap()
                .unwrap(),
            false,
            now,
        )
        .unwrap();
    let r = Readings::read(&state, ttl, now, wall).await;
    assert!(r.fresh.is_empty());
    assert_eq!(r.latest["drivetrain/soc"], "52");
    state.write().await.set_connected(true);
    assert_eq!(
        Readings::read(&state, ttl, now, wall).await.fresh["drivetrain/soc"],
        "52"
    );
    state
        .write()
        .await
        .ingest(
            decode("saic/test/vehicles/OTHER/drivetrain/soc", b"90", None)
                .unwrap()
                .unwrap(),
            false,
            now,
        )
        .unwrap();
    let r = Readings::read(&state, ttl, now, wall).await;
    assert!(r.latest.is_empty());
    assert!(r.fresh.is_empty());
}

#[tokio::test]
async fn notification_failure_cannot_change_climate_success_or_replay_commands() {
    let s = Server::new(vec![Reply(500, json!({}), None)]).await;
    let state = Arc::new(RwLock::new(Telemetry::default()));
    state.write().await.set_connected(true);
    state
        .write()
        .await
        .ingest(
            decode(
                "saic/test/vehicles/CAR/climate/interiorTemperature",
                b"20",
                None,
            )
            .unwrap()
            .unwrap(),
            false,
            Instant::now(),
        )
        .unwrap();
    let (controls, mut publishes) = Controls::new(state, Duration::from_secs(600));
    let starts = controls.preheating.subscribe();
    let caller = controls.clone();
    let command = tokio::spawn(async move { caller.execute("/climate/start", Some(20.)).await });
    let publish = publishes.recv().await.unwrap();
    let batch: Value = serde_json::from_str(&publish.payload).unwrap();
    assert!(starts.borrow().is_none());
    publish.ack.send(()).unwrap();
    controls.results.send(ResultMessage {topic:"saic/test/vehicles/CAR/commands/batch/result".into(),retained:false,value:json!({"requestId":batch["requestId"],"status":"success","results":[{"topic":"climate/remoteClimateState/set","status":"success"}]})}).ok().unwrap();
    assert!(command.await.unwrap().is_ok());
    let start = starts.borrow().unwrap();
    let mut worker = Activity::new(true, start);
    worker.start(start);
    worker
        .tick(&s.api, &s.url, &Readings::default(), start, Utc::now())
        .await;
    assert!(worker.deadline.is_some());
    assert!(publishes.try_recv().is_err());
    s.done();
}
