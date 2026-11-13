//! Optional Notify delivery. These workers cannot publish vehicle commands.
use crate::{telemetry::SharedTelemetry, telemetry::Source};
use chrono::{DateTime, Utc};
use reqwest::{Client, Method, Url};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tokio::sync::watch;

const CHARGING: &str = "Charging Big Red";
const PREHEATING: &str = "Preheating Big Red";

#[derive(Clone, Default)]
pub struct Config {
    pub dashboard: Option<Url>,
    pub activity: Option<Url>,
}
impl Config {
    pub fn load(
        mut get: impl FnMut(&str) -> Result<Option<String>, String>,
    ) -> Result<Self, String> {
        let mut read = |name| -> Result<Option<Url>, String> {
            get(name)?
                .filter(|s| !s.is_empty())
                .map(|s| {
                    Url::parse(&s)
                        .ok()
                        .filter(|u| {
                            matches!(u.scheme(), "http" | "https")
                                && u.host_str().is_some()
                                && u.username().is_empty()
                                && u.password().is_none()
                                && u.fragment().is_none()
                        })
                        .ok_or_else(|| {
                            format!("{name} must be an HTTP(S) URL without userinfo or fragment")
                        })
                })
                .transpose()
        };
        Ok(Self {
            dashboard: read("NOTIFY_DASHBOARD_URL")?,
            activity: read("NOTIFY_ACTIVITY_URL")?,
        })
    }
}

#[derive(Default)]
struct Readings {
    updated_at: Option<DateTime<Utc>>,
    latest: BTreeMap<String, Value>,
    fresh: BTreeMap<String, Value>,
}
impl Readings {
    async fn read(
        state: &SharedTelemetry,
        ttl: Duration,
        now: Instant,
        wall: DateTime<Utc>,
    ) -> Self {
        let state = state.read().await;
        let mut bases = state.vehicle_bases();
        let Some(base) = bases.next() else {
            return Self::default();
        };
        if bases.next().is_some() {
            return Self::default();
        }
        let source = Source::Vehicle(base.to_owned());
        let mut readings = Self {
            updated_at: state
                .latest(&source, "vehicle/refresh/lastVehicleState")
                .and_then(|r| r.value.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|s| s.with_timezone(&Utc)),
            ..Self::default()
        };
        for key in [
            "drivetrain/soc",
            "drivetrain/socTarget",
            "drivetrain/charging",
            "drivetrain/power",
            "drivetrain/remainingChargingTime",
            "drivetrain/chargerConnected",
            "drivetrain/running",
            "doors/locked",
            "climate/interiorTemperature",
            "climate/exteriorTemperature",
            "climate/remoteClimateState",
            "climate/heatedSeatsFrontLeftLevel",
            "climate/heatedSeatsFrontRightLevel",
            "climate/frontWindowDefrosterHeating",
            "climate/rearWindowDefrosterHeating",
        ] {
            let full = format!("vehicle/{key}");
            if let Some(r) = state.latest(&source, &full) {
                readings.latest.insert(key.into(), r.value.clone());
            }
            if let Some(v) = state.fresh(&source, &full, now, wall, ttl) {
                readings.fresh.insert(key.into(), v.clone());
            }
        }
        readings
    }
}
fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()))
        .filter(|n| n.is_finite())
}
fn boolean(value: Option<&Value>) -> Option<bool> {
    let value = value?;
    match value
        .as_str()
        .map(str::to_lowercase)
        .unwrap_or_else(|| value.to_string())
        .as_str()
    {
        "true" | "1" | "on" => Some(true),
        "false" | "0" | "off" => Some(false),
        _ => None,
    }
}
fn rounded(n: Option<f64>, suffix: &str) -> String {
    n.map_or_else(
        || "?".into(),
        |n| format!("{:.0}{suffix}", n.round_ties_even()),
    )
}
fn dashboard(r: &Readings) -> Value {
    let get = |k: &str| r.latest.get(k);
    let soc = number(get("drivetrain/soc"));
    let temp = number(get("climate/interiorTemperature"));
    let outside = number(get("climate/exteriorTemperature"));
    let locked = boolean(get("doors/locked"));
    let charging = boolean(get("drivetrain/charging"));
    let ac = get("climate/remoteClimateState")
        .map(|v| v.as_str().unwrap_or("").trim().to_lowercase() != "off");
    let status = if charging == Some(true) {
        "Charging"
    } else if boolean(get("drivetrain/chargerConnected")) == Some(true) {
        "Plugged in"
    } else if boolean(get("drivetrain/running")) == Some(true) {
        "Driving"
    } else {
        "Parked"
    };
    let stale = [
        "drivetrain/soc",
        "doors/locked",
        "drivetrain/charging",
        "climate/interiorTemperature",
        "climate/exteriorTemperature",
        "climate/remoteClimateState",
    ]
    .iter()
    .any(|k| !r.fresh.contains_key(*k));
    let unavailable = soc.is_none()
        || temp.is_none()
        || outside.is_none()
        || locked.is_none()
        || charging.is_none()
        || ac.is_none();
    let freshness = match r.updated_at {
        None if r.latest.is_empty() => "No data yet".to_owned(),
        None => "Telemetry unavailable".to_owned(),
        Some(stamp) => {
            let time = stamp
                .with_timezone(&chrono_tz::Europe::Brussels)
                .format("%d/%m/%Y %H:%M");
            let prefix = if stale {
                "Stale · "
            } else if unavailable {
                "Telemetry unavailable · "
            } else {
                ""
            };
            format!("{prefix}Updated at {time}")
        }
    };
    json!({"trailing": rounded(soc, "%"), "metrics": [
        {"color": "#22C55E", "label": "Doors", "value": locked.map_or("?", |v| if v {"Locked"} else {"Unlocked"})},
        {"color": "#22C55E", "label": "Status", "value": status},
        {"color": "#22C55E", "label": "Temperature", "value": format!("{} • {}", rounded(temp,"º"), rounded(outside,"º"))},
        {"color": "#22C55E", "label": "A/C", "value": ac.map_or("?", |v| if v {"On"} else {"Off"})}],
        "status": freshness})
}
fn climate(r: &Readings) -> Value {
    let get = |k: &str| r.latest.get(k);
    let temp = number(get("climate/interiorTemperature"));
    let mut body = vec![rounded(temp, "º")];
    if [
        "climate/heatedSeatsFrontLeftLevel",
        "climate/heatedSeatsFrontRightLevel",
    ]
    .iter()
    .any(|k| number(get(k)).unwrap_or(0.) > 0.)
    {
        body.push("Seat warmers".into());
    }
    if [
        "climate/frontWindowDefrosterHeating",
        "climate/rearWindowDefrosterHeating",
    ]
    .iter()
    .any(|k| boolean(get(k)) == Some(true))
    {
        body.push("Defrost".into());
    }
    json!({"title": PREHEATING, "body": body.join(" • "), "symbol": "flame.fill", "tint": "#EF4444", "steps": 20, "step": temp.map_or(0, |t| (t-1.).clamp(0.,20.) as u32)})
}
fn charging_payload(r: &Readings) -> Value {
    let get = |k: &str| r.fresh.get(k);
    let power = number(get("drivetrain/power")).map_or_else(|| "?".into(), |p| p.abs().to_string());
    let soc = number(get("drivetrain/soc"));
    let remaining = number(get("drivetrain/remainingChargingTime"))
        .filter(|n| *n > 0.)
        .map(|n| n.clamp(1., 86400.) as u64);
    json!({"title":CHARGING,"symbol":"bolt.car.fill","tint":"#EF4444","body":format!("{power} kW • {}",rounded(soc,"%")),"progress":soc,"status":"charging","endsIn":remaining})
}

// Never display reqwest errors: they may contain secret URL query parameters.
#[derive(Debug)]
struct Failure {
    gone: bool,
    delay: Duration,
}
impl Failure {
    fn ordinary() -> Self {
        Self {
            gone: false,
            delay: Duration::from_secs(30),
        }
    }
}
struct Api(Client);
impl Api {
    async fn request(
        &self,
        url: &Url,
        method: Method,
        payload: Option<&Value>,
    ) -> Result<Value, Failure> {
        let mut request = self.0.request(method, url.clone());
        if let Some(payload) = payload {
            request = request.json(payload);
        }
        let mut response = request.send().await.map_err(|_| Failure::ordinary())?;
        let status = response.status();
        if !status.is_success() {
            let delay = if status.as_u16() == 429 {
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(60)
                    .clamp(30, 3600)
            } else {
                30
            };
            return Err(Failure {
                gone: status.as_u16() == 410,
                delay: Duration::from_secs(delay),
            });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure::ordinary())? {
            if body.len() + chunk.len() > 256 * 1024 {
                return Err(Failure::ordinary());
            }
            body.extend_from_slice(&chunk);
        }
        if body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&body).map_err(|_| Failure::ordinary())
    }
    async fn find(&self, url: &Url, title: &str) -> Result<Option<Value>, Failure> {
        let list = self.request(url, Method::GET, None).await?;
        let items = list
            .get("activities")
            .and_then(Value::as_array)
            .ok_or_else(Failure::ordinary)?;
        if items.len() > 128 {
            return Err(Failure::ordinary());
        }
        let mut found = None;
        for item in items {
            let item = if item.get("content").is_none() {
                self.request(&activity_url(url, item)?, Method::GET, None)
                    .await?
            } else {
                item.clone()
            };
            if item["content"]["title"] == title {
                if found.is_some() {
                    return Err(Failure::ordinary());
                }
                found = Some(item);
            }
        }
        Ok(found)
    }
}
fn activity_url(url: &Url, item: &Value) -> Result<Url, Failure> {
    let id = item["activityId"]
        .as_str()
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .ok_or_else(Failure::ordinary)?;
    let mut url = url.clone();
    url.path_segments_mut()
        .map_err(|_| Failure::ordinary())?
        .pop()
        .push(id);
    Ok(url)
}

struct Dashboard {
    last: Option<Value>,
    sent: Instant,
    next: Instant,
}
impl Dashboard {
    fn new(now: Instant) -> Self {
        Self {
            last: None,
            sent: now,
            next: now,
        }
    }
    async fn tick(&mut self, api: &Api, url: &Url, r: &Readings, now: Instant) {
        if now < self.next {
            return;
        }
        self.next = now + Duration::from_secs(5);
        let payload = dashboard(r);
        if self.last.as_ref() == Some(&payload)
            && now.duration_since(self.sent) < Duration::from_secs(300)
        {
            return;
        }
        match api.request(url, Method::POST, Some(&payload)).await {
            Ok(_) => {
                self.last = Some(payload);
                self.sent = now;
            }
            Err(e) => {
                // Backoff starts after the response, including a slow discovery/request.
                self.next = Instant::now().max(now) + e.delay;
                tracing::warn!("Notify dashboard delivery failed");
            }
        }
    }
}
struct Activity {
    preheat: bool,
    target: Option<Url>,
    recovered: bool,
    suppressed: bool,
    deadline: Option<Instant>,
    next: Instant,
}
impl Activity {
    fn new(preheat: bool, now: Instant) -> Self {
        Self {
            preheat,
            target: None,
            recovered: false,
            suppressed: false,
            deadline: None,
            next: now,
        }
    }
    fn start(&mut self, started: Instant) {
        self.deadline = Some(started + Duration::from_secs(600));
        self.next = started;
    }
    async fn tick(
        &mut self,
        api: &Api,
        url: &Url,
        r: &Readings,
        now: Instant,
        wall: DateTime<Utc>,
    ) {
        if now < self.next {
            return;
        }
        self.next = now + Duration::from_secs(if self.preheat { 60 } else { 30 });
        if let Err(e) = self.update(api, url, r, now, wall).await {
            if e.gone {
                self.target = None;
                self.recovered = true;
                if self.preheat {
                    self.deadline = None;
                } else {
                    self.suppressed = true;
                }
            }
            // Backoff starts after the response, including a slow discovery/request.
            self.next = Instant::now().max(now) + e.delay;
            tracing::warn!("Notify Live Activity delivery failed");
        }
    }
    async fn update(
        &mut self,
        api: &Api,
        url: &Url,
        r: &Readings,
        now: Instant,
        wall: DateTime<Utc>,
    ) -> Result<(), Failure> {
        if !self.recovered {
            let found = api
                .find(url, if self.preheat { PREHEATING } else { CHARGING })
                .await?;
            if let Some(found) = found {
                self.target = Some(activity_url(url, &found)?);
                if self.preheat && self.deadline.is_none() {
                    let end = number(found["content"].get("endsAt"))
                        .or_else(|| {
                            found["endsAt"]
                                .as_str()
                                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                                .map(|d| d.timestamp() as f64)
                        })
                        .ok_or_else(Failure::ordinary)?;
                    self.deadline = Some(
                        now + Duration::from_secs_f64(
                            (end - wall.timestamp() as f64).clamp(0., 600.),
                        ),
                    );
                }
            } else {
                self.target = None;
            }
            self.recovered = true;
        }
        let mut payload;
        if self.preheat {
            let Some(deadline) = self.deadline else {
                return Ok(());
            };
            if now >= deadline {
                if let Some(target) = &self.target {
                    api.request(
                        target,
                        Method::DELETE,
                        Some(&json!({"status":"done","keepFor":0})),
                    )
                    .await?;
                }
                self.target = None;
                self.deadline = None;
                return Ok(());
            }
            payload = climate(r);
            payload["endsIn"] = json!(deadline.duration_since(now).as_secs().max(1));
            self.next = self.next.min(deadline);
        } else {
            let Some(charging) = boolean(r.fresh.get("drivetrain/charging")) else {
                return Ok(());
            };
            let soc = number(r.fresh.get("drivetrain/soc"));
            let target = number(r.fresh.get("drivetrain/socTarget"));
            if !charging || soc.zip(target).is_some_and(|(soc, target)| soc >= target) {
                if let Some(target) = &self.target {
                    api.request(target, Method::DELETE, Some(&json!({"status":if charging {"done"} else {"stopped"},"body":if charging {"Charging complete"} else {"Charging stopped"}}))).await?;
                }
                self.target = None;
                self.suppressed = charging;
                return Ok(());
            }
            if self.suppressed {
                return Ok(());
            }
            payload = charging_payload(r);
        }
        if let Some(target) = &self.target {
            api.request(target, Method::POST, Some(&payload)).await?;
        } else {
            self.recovered = false;
            let mut create = url.clone();
            create.query_pairs_mut().append_pair("new", "1");
            let result = api.request(&create, Method::POST, Some(&payload)).await?;
            self.target = Some(activity_url(url, &result)?);
            self.recovered = true;
        }
        Ok(())
    }
}

pub async fn run(
    config: Config,
    state: SharedTelemetry,
    ttl: Duration,
    mut starts: watch::Receiver<Option<Instant>>,
) {
    if config.dashboard.is_none() && config.activity.is_none() {
        std::future::pending::<()>().await;
    }
    let Ok(client) = Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("BigRed/1.0")
        .build()
    else {
        tracing::warn!("Notify HTTP client unavailable");
        std::future::pending::<()>().await;
        return;
    };
    let api = Api(client);
    let dashboard_worker = async {
        let Some(url) = config.dashboard else {
            std::future::pending::<()>().await;
            return;
        };
        let mut worker = Dashboard::new(Instant::now());
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let now = Instant::now();
            let r = Readings::read(&state, ttl, now, Utc::now()).await;
            worker.tick(&api, &url, &r, now).await;
        }
    };
    let charging_worker = async {
        let Some(url) = config.activity.as_ref() else {
            std::future::pending::<()>().await;
            return;
        };
        let mut worker = Activity::new(false, Instant::now());
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let now = Instant::now();
            let wall = Utc::now();
            let r = Readings::read(&state, ttl, now, wall).await;
            worker.tick(&api, url, &r, now, wall).await;
        }
    };
    let climate_worker = async {
        let Some(url) = config.activity.as_ref() else {
            std::future::pending::<()>().await;
            return;
        };
        let mut worker = Activity::new(true, Instant::now());
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if starts.has_changed().unwrap_or(false) {
                let start = *starts.borrow_and_update();
                if let Some(start) = start {
                    worker.start(start);
                }
            }
            let now = Instant::now();
            let wall = Utc::now();
            let r = Readings::read(&state, ttl, now, wall).await;
            worker.tick(&api, url, &r, now, wall).await;
        }
    };
    tokio::join!(dashboard_worker, charging_worker, climate_worker);
}

#[cfg(test)]
mod tests;
