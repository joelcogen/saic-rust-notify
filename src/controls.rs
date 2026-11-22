use crate::{
    telemetry::{ControlError, SharedTelemetry, Source, BATTERY_HEATING},
    vehicle::{Action, Request},
};
use chrono::Utc;
use serde_json::Value;
use std::{
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch, Mutex};

#[derive(Clone)]
pub struct Controls {
    pub state: SharedTelemetry,
    pub ttl: Duration,
    pub requests: mpsc::Sender<Request>,
    pub preheating: watch::Sender<Option<Instant>>,
    gate: Arc<Mutex<()>>,
}
pub const UNKNOWN: &str = "Vehicle command outcome unknown; earlier commands may have succeeded. Check vehicle state before retrying.";
impl Controls {
    pub fn new(state: SharedTelemetry, ttl: Duration) -> (Self, mpsc::Receiver<Request>) {
        let (requests, receiver) = mpsc::channel(16);
        let (preheating, _) = watch::channel(None);
        (
            Self {
                state,
                ttl,
                requests,
                preheating,
                gate: Arc::new(Mutex::new(())),
            },
            receiver,
        )
    }
    pub async fn execute(
        &self,
        path: &str,
        temperature: Option<f64>,
    ) -> Result<&'static str, &'static str> {
        if !matches!(
            path,
            "/refresh" | "/locate" | "/climate/start" | "/battery/heat"
        ) {
            return Err("Unknown vehicle control");
        }
        if temperature.is_some_and(|t| !t.is_finite()) {
            return Err("Temperature must be finite");
        }
        let _guard = self
            .gate
            .try_lock()
            .map_err(|_| "A vehicle control request is already in progress")?;
        let state = self.state.read().await;
        let vin = state
            .control_base()
            .map_err(|error| match error {
                ControlError::Disconnected => "SAIC session or vehicle discovery unavailable",
                ControlError::SingleVehicleRequired => "A single authorized vehicle is required",
                ControlError::DiscoveryIncomplete => "Vehicle discovery incomplete",
            })?
            .to_owned();
        if path == "/battery/heat"
            && state
                .fresh(
                    &Source::Vehicle(vin.clone()),
                    BATTERY_HEATING,
                    Instant::now(),
                    Utc::now(),
                    self.ttl,
                )
                .and_then(Value::as_bool)
                == Some(true)
        {
            return Ok("Battery heating is already on");
        }
        let temperature = if path == "/climate/start" && temperature.is_none() {
            let source = Source::Vehicle(vin.clone());
            Some(
                state
                    .fresh(
                        &source,
                        "vehicle/climate/interiorTemperature",
                        Instant::now(),
                        Utc::now(),
                        self.ttl,
                    )
                    .and_then(|v| parse_temperature(v).ok())
                    .ok_or("Fresh interior temperature is unavailable")?,
            )
        } else {
            temperature
        };
        drop(state);
        let action = match path {
            "/refresh" => Action::Refresh,
            "/locate" => Action::Locate,
            "/battery/heat" => Action::HeatBattery,
            _ => Action::Climate(temperature.unwrap_or(20.)),
        };
        let (reply, receive) = oneshot::channel();
        self.requests
            .try_send(Request { vin, action, reply })
            .map_err(|_| "Vehicle worker unavailable or busy")?;
        // Each client operation is bounded. If this wait is cancelled, the worker
        // finishes the accepted sequence without replaying it.
        let result = tokio::time::timeout(Duration::from_secs(360), receive)
            .await
            .map_err(|_| UNKNOWN)?
            .map_err(|_| UNKNOWN)??;
        if path == "/climate/start" {
            self.preheating.send_replace(Some(Instant::now()));
        }
        Ok(result)
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
pub fn climate_plan(temperature: f64) -> (Vec<saic_client::Command>, &'static str) {
    use saic_client::{Command, HeatedSeat, TemperatureProfile};
    let mut commands = vec![if temperature <= 4. {
        Command::start_front_defrost()
    } else {
        Command::start_ac(TemperatureProfile::Standard, 22).expect("valid fixed target")
    }];
    if temperature <= 6. {
        commands.push(Command::control_heated_seat(HeatedSeat::FrontLeft, 3).expect("valid level"));
        commands
            .push(Command::control_heated_seat(HeatedSeat::FrontRight, 3).expect("valid level"));
        commands.push(Command::set_steering_wheel_heater(true));
    }
    if temperature <= 4. {
        commands.push(Command::set_rear_window_heater(true));
    }
    (
        commands,
        if temperature <= 4. {
            "Heating up and defrosting"
        } else if temperature <= 6. {
            "Heating up and warming seats"
        } else {
            "Heating up"
        },
    )
}
