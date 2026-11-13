use crate::{mqtt::SharedTelemetry, telemetry::Source};
use chrono::Utc;
use serde_json::{json, Value};
use std::{
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex};

pub struct Publish {
    pub topic: String,
    pub payload: String,
    pub ack: oneshot::Sender<()>,
}
#[derive(Clone)]
pub struct ResultMessage {
    pub topic: String,
    pub value: Value,
    pub retained: bool,
}
#[derive(Clone)]
pub struct Controls {
    pub state: SharedTelemetry,
    pub ttl: Duration,
    pub publishes: mpsc::Sender<Publish>,
    pub results: broadcast::Sender<ResultMessage>,
    /// Notify integrations may observe this only after acknowledged full success.
    pub preheating: watch::Sender<Option<Instant>>,
    gate: Arc<Mutex<()>>,
}
pub const UNKNOWN: &str = "Vehicle command outcome unknown; earlier commands may have succeeded. Check vehicle state before retrying.";
impl Controls {
    pub fn new(state: SharedTelemetry, ttl: Duration) -> (Self, mpsc::Receiver<Publish>) {
        let (publishes, receiver) = mpsc::channel(16);
        let (results, _) = broadcast::channel(128);
        let (preheating, _) = watch::channel(None);
        (
            Self {
                state,
                ttl,
                publishes,
                results,
                preheating,
                gate: Arc::new(Mutex::new(())),
            },
            receiver,
        )
    }
    async fn publish(&self, topic: String, payload: String) -> Result<(), &'static str> {
        let (ack, receive) = oneshot::channel();
        tokio::time::timeout(Duration::from_secs(5), async {
            self.publishes
                .send(Publish {
                    topic,
                    payload,
                    ack,
                })
                .await
                .map_err(|_| UNKNOWN)?;
            receive.await.map_err(|_| UNKNOWN)
        })
        .await
        .map_err(|_| "MQTT publish acknowledgment timed out; outcome unknown; earlier commands may have succeeded. Check vehicle state before retrying.")?
    }
    pub async fn execute(
        &self,
        path: &str,
        temperature: Option<f64>,
    ) -> Result<&'static str, &'static str> {
        if !matches!(path, "/refresh" | "/locate" | "/climate/start") {
            return Err("Unknown vehicle control");
        }
        if temperature.is_some_and(|t| !t.is_finite()) {
            return Err("Temperature must be finite");
        }
        let _guard = if path != "/refresh" {
            Some(
                self.gate
                    .try_lock()
                    .map_err(|_| "A vehicle control request is already in progress")?,
            )
        } else {
            None
        };
        let state = self.state.read().await;
        let base = state
            .control_base()
            .map_err(|_| "MQTT broker unavailable or a single vehicle is not known")?
            .to_owned();
        let temperature = if path == "/climate/start" && temperature.is_none() {
            let source = Source::Vehicle(base.clone());
            ["interior", "exterior"]
                .iter()
                .filter_map(|side| {
                    state
                        .fresh(
                            &source,
                            &format!("vehicle/climate/{side}Temperature"),
                            Instant::now(),
                            Utc::now(),
                            self.ttl,
                        )
                        .and_then(|v| parse_temperature(v).ok())
                })
                .reduce(f64::min)
                .ok_or("Fresh vehicle temperature is unavailable")?
                .into()
        } else {
            temperature
        };
        drop(state);
        if path == "/refresh" {
            self.publish(format!("{base}/refresh/mode/set"), "force".into())
                .await?;
            return Ok("Refresh requested");
        }
        let mut results = self.results.subscribe();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let (commands, response) = climate_plan(temperature.unwrap_or(20.0));
        let climate = path == "/climate/start";
        let (topic, payload, seconds) = if climate {
            (
                "commands/batch",
                json!({"requestId": id, "commands": commands}).to_string(),
                360,
            )
        } else {
            ("location/findMyCar", "lights_only".into(), 45)
        };
        self.publish(format!("{base}/{topic}/set"), payload).await?;
        let expected = Value::Array(
            commands
                .iter()
                .map(|c| json!({"topic": c["topic"], "status": "success"}))
                .collect(),
        );
        let result_topic = format!("{base}/{topic}/result");
        let wait = async {
            loop {
                tokio::select! {
                    result = results.recv() => {
                        let result = result.map_err(|_| UNKNOWN)?;
                        if result.retained || result.topic != result_topic || (climate && result.value["requestId"] != id) { continue; }
                        if !climate {
                            return if result.value == "Success" {Ok(())} else {Err("Vehicle rejected locate; earlier commands may have succeeded.")};
                        }
                        if result.value["status"] == "success" && result.value["results"] == expected { return Ok(()); }
                        if result.value["status"] == "unknown" || result.value["results"].as_array().is_some_and(|r| r.iter().any(|v| v["status"] == "unknown")) { return Err(UNKNOWN); }
                        return Err(if result.value["results"].as_array().is_some_and(|r| r.iter().any(|v| v["status"] == "success")) {
                            "Climate batch partial failure; earlier commands may have succeeded. Check vehicle state before retrying."
                        } else { "Climate batch rejected or incomplete; earlier commands may have succeeded. Check vehicle state before retrying." });
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if !self.state.read().await.is_connected() { return Err(UNKNOWN); }
                    }
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(seconds), wait)
            .await
            .map_err(|_| "Vehicle command result timed out; outcome unknown; earlier commands may have succeeded. Check vehicle state before retrying.")??;
        if climate {
            self.preheating.send_replace(Some(Instant::now()));
            Ok(response)
        } else {
            Ok("Flashing lights")
        }
    }
}

pub fn parse_temperature(value: &Value) -> Result<f64, &'static str> {
    static PATTERN: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)^\s*([+-]?(?:[0-9]+(?:[.,][0-9]*)?|[.,][0-9]+))\s*(?:[°º]\s*)?([CF])?\s*$",
        )
        .unwrap()
    });
    let invalid =
        "temperature must be a finite Celsius number or temperature text with optional °C/°F";
    let number = if let Some(text) = value.as_str() {
        let captures = PATTERN.captures(text).ok_or(invalid)?;
        let n = captures[1]
            .replace(',', ".")
            .parse::<f64>()
            .map_err(|_| invalid)?;
        if captures
            .get(2)
            .is_some_and(|m| m.as_str().eq_ignore_ascii_case("f"))
        {
            (n - 32.0) * 5.0 / 9.0
        } else {
            n
        }
    } else {
        value.as_f64().ok_or(invalid)?
    };
    if number.is_finite() {
        Ok(number)
    } else {
        Err(invalid)
    }
}
pub fn climate_plan(temperature: f64) -> (Vec<Value>, &'static str) {
    let mut commands = vec![
        json!({"topic": if temperature <= 4.0 {"climate/frontWindowDefrosterHeating/set"} else {"climate/remoteClimateState/set"}, "payload": "on"}),
    ];
    if temperature <= 6.0 {
        for (topic, payload) in [
            ("heatedSeatsFrontLeftLevel", "3"),
            ("heatedSeatsFrontRightLevel", "3"),
            ("steeringWheelHeating", "on"),
        ] {
            commands.push(json!({"topic": format!("climate/{topic}/set"), "payload": payload}));
        }
    }
    if temperature <= 4.0 {
        commands.push(json!({"topic": "climate/rearWindowDefrosterHeating/set", "payload": "on"}));
    }
    (
        commands,
        if temperature <= 4.0 {
            "Heating up and defrosting"
        } else if temperature <= 6.0 {
            "Heating up and warming seats"
        } else {
            "Heating up"
        },
    )
}
