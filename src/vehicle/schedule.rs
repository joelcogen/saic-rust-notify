//! Fixed gateway policy, with a staged parked cadence. Times are UTC seconds;
//! the worker supplies a monotonic clock anchored to UTC for restart persistence.
use chrono::{Local, TimeZone};
use saic_client::models::{ChrgMgmtDataResp, VehicleStatusResp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io, path::PathBuf};

pub const ACTIVE: i64 = 30;
pub const SHUTDOWN: i64 = 120;
pub const GRACE: i64 = 600;
pub const HALF_DAY: i64 = 12 * 3600;
pub const DAY: i64 = 24 * 3600;
pub const WINDOW: i64 = 48 * 3600;
const ALARM_REFRESH_GAP: i64 = 60;

#[derive(Default)]
pub struct Observation {
    pub active: bool,
    pub charging: bool,
    pub battery_heating: bool,
    pub power: Option<f64>,
    pub capacity: Option<f64>,
    pub remaining: Option<i64>,
    pub scheduled: Option<i64>,
}
impl Observation {
    pub fn read(vehicle: &VehicleStatusResp, charge: &ChrgMgmtDataResp, now: i64) -> Self {
        let mut result = Self {
            active: vehicle.basic_vehicle_status.as_ref().is_some_and(|b| {
                b.engine_status == Some(1)
                    || b.remote_climate_status.is_some_and(|s| s > 0)
                    || b.rmt_htd_rr_wnd_st.is_some_and(|s| s > 0)
            }),
            capacity: charge
                .rvs_charge_status
                .as_ref()
                .and_then(|b| b.total_battery_capacity)
                .filter(|n| *n > 0)
                .map(|n| n as f64 / 10.),
            ..Self::default()
        };
        let Some(b) = &charge.chrg_mgmt_data else {
            return result;
        };
        result.charging = matches!(b.bms_chrg_sts, Some(1 | 3 | 10 | 12));
        result.battery_heating = super::battery_heating(b.bms_ptc_heat_req_dsp_cmd) == Some(true);
        if b.bms_pack_crnt_v == Some(0) {
            if let (Some(current @ 0..=65535), Some(voltage @ 0..=65535)) =
                (b.bms_pack_crnt, b.bms_pack_vol)
            {
                result.power =
                    Some((current as f64 * 0.05 - 1000.) * voltage as f64 * 0.25 / 1000.);
            }
        }
        if b.chrgng_rmnng_time_v == Some(0) {
            result.remaining = b
                .chrgng_rmnng_time
                .filter(|v| (1..=65535).contains(v))
                .map(|v| v * 60);
        }
        if matches!(b.bms_reser_ctrl_dsp_cmd, Some(1 | 3)) {
            if let (Some(h @ 0..=23), Some(m @ 0..=59)) =
                (b.bms_reser_st_hour_dsp_cmd, b.bms_reser_st_mintue_dsp_cmd)
            {
                result.scheduled = next_charge_check(now, h as u32, m as u32);
            }
        }
        result
    }
}

fn next_charge_check(now: i64, hour: u32, minute: u32) -> Option<i64> {
    let date = Local.timestamp_opt(now, 0).single()?.date_naive();
    // Gateway default: local timezone, scheduled start plus ten minutes.
    (-1..=2)
        .filter_map(|days| {
            let start = (date + chrono::Duration::days(days)).and_hms_opt(hour, minute, 0)?
                + chrono::Duration::seconds(GRACE);
            let time = Local.from_local_datetime(&start).earliest()?.timestamp();
            (time > now).then_some(time)
        })
        .min()
}

#[derive(Default, Serialize, Deserialize)]
pub struct Schedule {
    pub parked_since: Option<i64>,
    last_poll: Option<i64>,
    last_activity: Option<i64>,
    failures: u32,
    charging_interval: i64,
    pub next: i64,
    scheduled: Option<i64>,
    #[serde(skip)]
    alarm_due: Option<i64>,
}
impl Schedule {
    pub fn due(&self) -> i64 {
        // New activity takes precedence over recovery; scheduled detection is
        // the gateway's one-shot force refresh, consumed by any subsequent poll.
        if self.last_activity > self.last_poll {
            return self.last_activity.unwrap();
        }
        [self.scheduled, self.alarm_due]
            .into_iter()
            .flatten()
            .fold(self.next, i64::min)
    }
    pub fn note_alarm(&mut self, now: i64) {
        if self.failures > 0 || self.charging_interval > 0 {
            return;
        }
        let due = self
            .last_poll
            .map_or(now, |last| now.max(last + ALARM_REFRESH_GAP));
        if due < self.due() {
            self.alarm_due = Some(due);
        }
    }
    fn inactive_interval(&self, now: i64) -> i64 {
        if self.parked_since.is_some_and(|epoch| now - epoch < WINDOW) {
            HALF_DAY
        } else {
            DAY
        }
    }
    pub fn success(&mut self, now: i64, observation: Observation) {
        self.last_poll = Some(now);
        self.alarm_due = None;
        self.failures = 0;
        self.scheduled = observation.scheduled;
        if observation.active || observation.charging {
            self.last_activity = Some(now);
            self.parked_since = None;
        } else {
            // First observation of an inactive vehicle starts a conservative
            // known parking epoch; subsequent reads never move it.
            self.parked_since.get_or_insert(now);
        }
        if observation.charging {
            self.charging_interval = match (observation.power, observation.capacity) {
                (Some(power), Some(capacity)) if power < -1. => {
                    let one_percent = 36. * capacity / power.abs();
                    observation
                        .remaining
                        .map_or(one_percent, |r| (r as f64).min(one_percent.ceil()))
                        .clamp(ACTIVE as f64, self.inactive_interval(now) as f64)
                        .round_ties_even() as i64
                }
                _ => self.charging_interval.max(SHUTDOWN),
            };
            self.next = now + self.charging_interval;
        } else if observation.active {
            self.charging_interval = 0;
            self.next = now + ACTIVE;
        } else {
            self.charging_interval = 0;
            let epoch = self.parked_since.unwrap();
            let age = (now - epoch).max(0);
            self.next = if age + SHUTDOWN < GRACE {
                now + SHUTDOWN
            } else if age < WINDOW {
                epoch + (age / HALF_DAY + 1) * HALF_DAY
            } else {
                // The 48-hour poll is followed by the first 24-hour interval.
                epoch + WINDOW + ((age - WINDOW) / DAY + 1) * DAY
            };
        }
        if observation.battery_heating {
            self.next = self.next.min(now + ACTIVE);
        }
    }
    pub fn failure(&mut self, now: i64) {
        self.last_poll = Some(now);
        self.alarm_due = None;
        let delay = ACTIVE
            .saturating_mul(1i64 << self.failures.min(30))
            .min(self.inactive_interval(now));
        self.failures = self.failures.saturating_add(1);
        self.next = now + delay;
        if self.scheduled.is_some_and(|s| s <= now) {
            self.scheduled = None;
        }
    }
}

#[derive(Default)]
pub struct Schedules {
    entries: BTreeMap<String, Schedule>,
    path: Option<PathBuf>,
}
impl Schedules {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let entries = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        let result = Self {
            entries,
            path: Some(path),
        };
        result.save()?; // Check persistence before any vehicle access.
        Ok(result)
    }
    pub fn vehicle(&mut self, vin: &str) -> &mut Schedule {
        self.entries
            .entry(format!("{:x}", Sha256::digest(vin.as_bytes())))
            .or_default()
    }
    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("state directory missing"))?;
        fs::create_dir_all(parent)?;
        let temp = path.with_extension("tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&temp)?;
        serde_json::to_writer(&file, &self.entries).map_err(io::Error::other)?;
        file.sync_all()?;
        fs::rename(temp, path)?;
        fs::File::open(parent)?.sync_all()
    }
}

#[cfg(test)]
mod tests;
