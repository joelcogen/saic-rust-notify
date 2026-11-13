use crate::config::valid_segment;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub const MAX_PAYLOAD: usize = 256 * 1024;
const MAX_READINGS: usize = 4096;
const MAX_SOURCES: usize = 128;
const MAX_STATE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Account(String),
    Vehicle(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    Topic,
    Utf8,
    Json,
    TooLarge,
    Capacity,
}

pub struct Message {
    pub source: Source,
    pub key: String,
    pub value: Value,
}

pub fn decode(
    topic: &str,
    payload: &[u8],
    account: Option<&str>,
) -> Result<Option<Message>, Reject> {
    if topic.len() > 4096 || payload.len() > MAX_PAYLOAD {
        return Err(Reject::TooLarge);
    }
    let parts: Vec<_> = topic.split('/').collect();
    if parts.len() < 3 || parts[0] != "saic" || parts.iter().any(|s| !valid_segment(s)) {
        return Err(Reject::Topic);
    }
    if account.is_some_and(|a| a != parts[1]) {
        return Ok(None);
    }
    let (source, key) = if parts[2] == "vehicles" {
        if parts.len() < 4 {
            return Err(Reject::Topic);
        }
        (
            Source::Vehicle(parts[..4].join("/")),
            format!(
                "vehicle/{}",
                if parts.len() == 4 {
                    "root".into()
                } else {
                    parts[4..].join("/")
                }
            ),
        )
    } else {
        (
            Source::Account(parts[1].into()),
            format!("account/{}", parts[2..].join("/")),
        )
    };
    let text = std::str::from_utf8(payload).map_err(|_| Reject::Utf8)?;
    // Gateway scalar values are text; structured payloads must be valid JSON.
    let value = if text.trim_start().starts_with(['{', '[']) {
        serde_json::from_str(text).map_err(|_| Reject::Json)?
    } else {
        Value::String(text.into())
    };
    Ok(Some(Message { source, key, value }))
}

#[derive(Clone)]
pub struct Reading {
    pub value: Value,
    pub retained: bool,
    pub received_at: Instant,
    stored_bytes: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ControlError {
    Disconnected,
    SingleVehicleRequired,
    DiscoveryIncomplete,
}

#[derive(Default)]
pub struct Telemetry {
    connected: bool,
    sources: BTreeMap<Source, BTreeMap<String, Reading>>,
    last_message_at: Option<Instant>,
    stale_warning_emitted: bool,
    capacity_exceeded: bool,
    stored_bytes: usize,
}

impl Telemetry {
    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }
    pub fn is_connected(&self) -> bool {
        self.connected
    }
    pub fn vehicle_bases(&self) -> impl Iterator<Item = &str> {
        self.sources.keys().filter_map(|s| match s {
            Source::Vehicle(base) => Some(base.as_str()),
            _ => None,
        })
    }
    pub fn control_base(&self) -> Result<&str, ControlError> {
        if !self.connected {
            return Err(ControlError::Disconnected);
        }
        if self.capacity_exceeded {
            return Err(ControlError::DiscoveryIncomplete);
        }
        let mut bases = self.vehicle_bases();
        let base = bases.next().ok_or(ControlError::SingleVehicleRequired)?;
        if bases.next().is_some() {
            return Err(ControlError::SingleVehicleRequired);
        }
        Ok(base)
    }
    /// Returns whether accepted traffic resumed after a stale warning.
    pub fn ingest(
        &mut self,
        message: Message,
        retained: bool,
        now: Instant,
    ) -> Result<bool, Reject> {
        let previous = self.latest(&message.source, &message.key);
        let stored_bytes = message.value.to_string().len() + message.key.len();
        let new_size = self.stored_bytes - previous.map_or(0, |r| r.stored_bytes) + stored_bytes;
        let new_key = previous.is_none();
        if (!self.sources.contains_key(&message.source) && self.sources.len() >= MAX_SOURCES)
            || (new_key && self.sources.values().map(BTreeMap::len).sum::<usize>() >= MAX_READINGS)
            || new_size > MAX_STATE_BYTES
        {
            // Never authorize controls if resource limits could hide another vehicle.
            self.capacity_exceeded = true;
            return Err(Reject::Capacity);
        }
        self.stored_bytes = new_size;
        let readings = self.sources.entry(message.source).or_default();
        readings.insert(
            message.key,
            Reading {
                value: message.value,
                retained,
                received_at: now,
                stored_bytes,
            },
        );
        self.last_message_at = Some(now);
        let resumed = self.stale_warning_emitted;
        self.stale_warning_emitted = false;
        Ok(resumed)
    }
    pub fn latest(&self, source: &Source, key: &str) -> Option<&Reading> {
        self.sources.get(source)?.get(key)
    }
    pub fn fresh(
        &self,
        source: &Source,
        key: &str,
        now: Instant,
        wall: DateTime<Utc>,
        ttl: Duration,
    ) -> Option<&Value> {
        if !self.connected {
            return None;
        }
        let reading = self.latest(source, key)?;
        let charge = matches!(
            key,
            "vehicle/drivetrain/soc"
                | "vehicle/drivetrain/socTarget"
                | "vehicle/drivetrain/charging"
                | "vehicle/drivetrain/power"
                | "vehicle/drivetrain/remainingChargingTime"
                | "vehicle/drivetrain/chargerConnected"
        );
        let stamp_key = if charge {
            "vehicle/refresh/lastChargeState"
        } else {
            "vehicle/refresh/lastVehicleState"
        };
        if let Some(stamp) = self
            .latest(source, stamp_key)
            .and_then(|r| r.value.as_str())
        {
            if let Ok(stamp) = DateTime::parse_from_rfc3339(stamp) {
                if wall
                    .signed_duration_since(stamp)
                    .to_std()
                    .is_ok_and(|age| age <= ttl)
                {
                    return Some(&reading.value);
                }
            } else if chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
                || chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S%.f").is_ok()
            {
                // Preserve the reference's explicit rejection of timezone-free timestamps.
                return None;
            }
        }
        if !reading.retained
            && now
                .checked_duration_since(reading.received_at)
                .is_some_and(|age| age <= ttl)
        {
            Some(&reading.value)
        } else {
            None
        }
    }
    /// Emit a single warning until another accepted message arrives, as in Python.
    pub fn check_stale(&mut self, now: Instant, ttl: Duration) -> bool {
        if !self.stale_warning_emitted
            && self
                .last_message_at
                .is_none_or(|last| now.saturating_duration_since(last) >= ttl)
        {
            self.stale_warning_emitted = true;
            true
        } else {
            false
        }
    }
}
