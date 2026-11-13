//! Direct SAIC session worker. Only this worker owns the client, so polling cannot
//! interleave a climate sequence and an uncertain command is never replayed.
use crate::{
    config::Config,
    controls::{climate_plan, UNKNOWN},
    telemetry::{Message, SharedTelemetry, Source},
};
use saic_client::{
    models::{ChrgMgmtDataResp, VehicleListResp, VehicleStatusResp},
    ClientConfig, Command, CommandOptions, CommandState, Credentials, FindCarMode, SaicClient,
};
use serde_json::{json, Value};
use std::{
    future::Future,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

mod schedule;
use schedule::{Observation, Schedules};

pub enum Action {
    Refresh,
    Locate,
    Climate(f64),
}
pub struct Request {
    pub vin: String,
    pub action: Action,
    pub reply: oneshot::Sender<Result<&'static str, &'static str>>,
}

// A narrow boundary lets offline tests exercise the real scheduler and command
// sequencing with typed client responses, without credentials or network calls.
trait Session {
    async fn login(&mut self) -> Result<(), ()>;
    async fn vehicles(&mut self) -> Result<VehicleListResp, ()>;
    async fn vehicle(&mut self, vin: &str) -> Result<VehicleStatusResp, ()>;
    async fn charge(&mut self, vin: &str) -> Result<ChrgMgmtDataResp, ()>;
    async fn command(&mut self, vin: &str, command: &Command) -> Result<CommandState, ()>;
}
impl Session for SaicClient {
    async fn login(&mut self) -> Result<(), ()> {
        SaicClient::login(self).await.map_err(|_| ())
    }
    async fn vehicles(&mut self) -> Result<VehicleListResp, ()> {
        SaicClient::vehicles(self).await.map_err(|_| ())
    }
    async fn vehicle(&mut self, vin: &str) -> Result<VehicleStatusResp, ()> {
        self.vehicle_status_live(vin, CommandOptions::default())
            .await
            .map_err(|_| ())
    }
    async fn charge(&mut self, vin: &str) -> Result<ChrgMgmtDataResp, ()> {
        self.charging_management(vin, CommandOptions::default())
            .await
            .map_err(|_| ())
    }
    async fn command(&mut self, vin: &str, command: &Command) -> Result<CommandState, ()> {
        self.execute_command(vin, command, CommandOptions::default())
            .await
            .map(|o| o.state)
            .map_err(|_| ())
    }
}

pub fn client(config: &Config) -> Result<SaicClient, &'static str> {
    let mut client_config =
        ClientConfig::mg_europe(Credentials::email(&config.username, &config.password));
    if let Some(url) = &config.api_base_url {
        client_config = client_config.with_base_url(url);
    }
    SaicClient::new(client_config)
        .map_err(|_| "Invalid SAIC_BASE_URL or SAIC account configuration")
}

pub async fn run(
    mut client: SaicClient,
    config: Config,
    state: SharedTelemetry,
    requests: mpsc::Receiver<Request>,
    shutdown: impl Future<Output = ()>,
) {
    let mut schedules = match Schedules::open("data/polling.json".into()) {
        Ok(schedules) => schedules,
        Err(_) => {
            tracing::error!("Polling state unavailable; refusing vehicle access");
            return;
        }
    };
    tokio::select! {
        _ = worker(&mut client, &config, &state, requests, &mut schedules) => {},
        _ = shutdown => {},
    }
    state.write().await.set_connected(false);
}

fn select_vehicles(list: VehicleListResp, selected: Option<&str>) -> Result<Vec<String>, ()> {
    let entries = list.vin_list.ok_or(())?;
    if entries.len() > 128 {
        return Err(());
    }
    let mut vins = Vec::new();
    for entry in entries {
        let vin = entry
            .vin
            .filter(|v| crate::config::valid_segment(v))
            .ok_or(())?;
        if selected.is_none_or(|wanted| wanted == vin) && !vins.contains(&vin) {
            vins.push(vin);
        }
    }
    if vins.is_empty() {
        Err(())
    } else {
        Ok(vins)
    }
}

async fn worker(
    client: &mut impl Session,
    config: &Config,
    state: &SharedTelemetry,
    mut requests: mpsc::Receiver<Request>,
    schedules: &mut Schedules,
) {
    let start = tokio::time::Instant::now();
    let epoch = chrono::Utc::now().timestamp();
    let now = || epoch + start.elapsed().as_secs() as i64;
    let mut vins = Vec::new();
    let mut authenticated = false;
    loop {
        if !authenticated {
            state.write().await.set_connected(false);
            let discovered = async {
                client.login().await?;
                select_vehicles(client.vehicles().await?, config.vin.as_deref())
            }
            .await;
            let Ok(discovered) = discovered else {
                tracing::warn!("SAIC login or vehicle discovery unavailable; retrying");
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            };
            vins = discovered;
            {
                let mut state = state.write().await;
                state.select_vehicles(&vins);
                // Session readiness does not require a telemetry read. Failed
                // sources and observation ages still govern freshness.
                state.set_connected(true);
            }
            authenticated = true;
        }
        let due = vins
            .iter()
            .map(|vin| schedules.vehicle(vin).due())
            .min()
            .unwrap();
        let wait = Duration::from_secs(due.saturating_sub(now()).max(0) as u64);
        tokio::select! {
            request = requests.recv() => {
                let Some(request) = request else { return; };
                if request.reply.is_closed() { continue; }
                if vins.len() != 1 || vins[0] != request.vin {
                    let _ = request.reply.send(Err("A single authorized vehicle is required"));
                    continue;
                }
                let result = if matches!(request.action, Action::Refresh) {
                    poll_vehicle(client, state, &request.vin, schedules, &now).await
                        .map(|()| "Vehicle refreshed").map_err(|()| "Vehicle refresh failed")
                } else {
                    // Controls never change polling deadlines or parking age.
                    execute(client, state, &request.vin, request.action).await
                };
                if result.is_err() {
                    state.write().await.fail_vehicle(&request.vin);
                    state.write().await.set_connected(false);
                    authenticated = false;
                }
                let _ = request.reply.send(result);
            }
            _ = tokio::time::sleep(wait) => {
                // Each due vehicle gets at most one attempt. Deadlines are based
                // on completion, so delayed operations cannot cause catch-up bursts.
                for vin in &vins {
                    if schedules.vehicle(vin).due() <= now() {
                        if poll_vehicle(client, state, vin, schedules, &now).await.is_err() {
                            state.write().await.set_connected(false);
                            authenticated = false;
                        }
                        // Persist before another vehicle's potentially slow read.
                        if schedules.save().is_err() {
                            tracing::error!("Could not persist polling state; stopping vehicle access");
                            return;
                        }
                    }
                }
            }
        }
        if !authenticated {
            while let Ok(request) = requests.try_recv() {
                let _ = request
                    .reply
                    .send(Err("SAIC unavailable; queued request cancelled"));
            }
        }
        if schedules.save().is_err() {
            tracing::error!("Could not persist polling state; stopping vehicle access");
            return;
        }
    }
}

async fn poll_vehicle(
    client: &mut impl Session,
    state: &SharedTelemetry,
    vin: &str,
    schedules: &mut Schedules,
    now: &impl Fn() -> i64,
) -> Result<(), ()> {
    match refresh(client, state, vin).await {
        Ok(observation) => {
            schedules.vehicle(vin).success(now(), observation);
            Ok(())
        }
        Err(()) => {
            state.write().await.fail_vehicle(vin);
            schedules.vehicle(vin).failure(now());
            Err(())
        }
    }
}

async fn execute(
    client: &mut impl Session,
    state: &SharedTelemetry,
    vin: &str,
    action: Action,
) -> Result<&'static str, &'static str> {
    let (commands, message) = match action {
        Action::Refresh => {
            refresh(client, state, vin)
                .await
                .map_err(|_| "Vehicle refresh failed")?;
            return Ok("Vehicle refreshed");
        }
        Action::Locate => (
            vec![Command::find_my_car(FindCarMode::LightsOnly)],
            "Flashing lights",
        ),
        Action::Climate(temperature) => climate_plan(temperature),
    };
    for (index, command) in commands.iter().enumerate() {
        match client.command(vin, command).await {
            Ok(CommandState::Completed) => {},
            Ok(CommandState::Rejected) if index == 0 => return Err("Vehicle command rejected"),
            Ok(CommandState::Rejected) => return Err("Climate partial failure; earlier commands succeeded. Check vehicle state before retrying."),
            _ => return Err(UNKNOWN),
        }
    }
    Ok(message)
}

async fn refresh(
    client: &mut impl Session,
    state: &SharedTelemetry,
    vin: &str,
) -> Result<Observation, ()> {
    let vehicle = client.vehicle(vin).await?;
    if !valid_vehicle_timestamp(vehicle.status_time, chrono::Utc::now().timestamp()) {
        tracing::warn!("Vehicle status timestamp invalid or outside 15-minute window");
        return Err(());
    }
    let charge = client.charge(vin).await?;
    let observation = Observation::read(&vehicle, &charge, chrono::Utc::now().timestamp());
    let now = Instant::now();
    let mut state = state.write().await;
    let source = Source::Vehicle(vin.into());
    // Replace the snapshot, so a new timestamp cannot certify a missing old field.
    state.clear_vehicle(vin);
    for (key, value) in vehicle_readings(vehicle) {
        state
            .ingest(
                Message {
                    source: source.clone(),
                    key: format!("vehicle/{key}"),
                    value,
                },
                true,
                now,
            )
            .map_err(|_| ())?;
    }
    for (key, value) in charge_readings(charge) {
        // Management data has no source timestamp; use monotonic receipt age.
        state
            .ingest(
                Message {
                    source: source.clone(),
                    key: format!("vehicle/{key}"),
                    value,
                },
                false,
                now,
            )
            .map_err(|_| ())?;
    }
    state.set_connected(true);
    Ok(observation)
}

// Match the gateway's invalid sentinels and inclusive 15-minute drift limit.
fn valid_vehicle_timestamp(stamp: Option<i64>, now: i64) -> bool {
    stamp.is_some_and(|stamp| {
        !matches!(stamp, 0 | 2_147_483_647)
            && chrono::DateTime::from_timestamp(stamp, 0).is_some()
            && now.abs_diff(stamp) <= 15 * 60
    })
}

type Readings = Vec<(&'static str, Value)>;
fn vehicle_readings(status: VehicleStatusResp) -> Readings {
    let mut readings = Vec::new();
    if let Some(stamp) = status
        .status_time
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
    {
        readings.push(("refresh/lastVehicleState", json!(stamp.to_rfc3339())));
    }
    let Some(b) = status.basic_vehicle_status else {
        return readings;
    };
    for (key, value) in [
        ("climate/interiorTemperature", b.interior_temperature),
        ("climate/exteriorTemperature", b.exterior_temperature),
    ] {
        if let Some(v) = value.filter(|v| (-127..=127).contains(v) && *v != 87) {
            readings.push((key, json!(v)));
        }
    }
    for (key, value) in [
        ("doors/locked", b.lock_status),
        ("drivetrain/running", b.engine_status),
    ] {
        if let Some(v @ 0..=1) = value {
            readings.push((key, json!(v == 1)));
        }
    }
    for (key, value) in [
        (
            "climate/heatedSeatsFrontLeftLevel",
            b.front_left_seat_heat_level,
        ),
        (
            "climate/heatedSeatsFrontRightLevel",
            b.front_right_seat_heat_level,
        ),
    ] {
        if let Some(v @ 0..=3) = value {
            readings.push((key, json!(v)));
        }
    }
    if let Some(v @ 0..=1) = b.rmt_htd_rr_wnd_st {
        readings.push(("climate/rearWindowDefrosterHeating", json!(v == 1)));
    }
    if let Some(mode) = b.remote_climate_status.and_then(|v| match v {
        0 => Some("off"),
        1 => Some("blowingonly"),
        2 => Some("on"),
        5 => Some("front"),
        _ => None,
    }) {
        readings.push(("climate/remoteClimateState", json!(mode)));
        readings.push((
            "climate/frontWindowDefrosterHeating",
            json!(mode == "front"),
        ));
    }
    readings
}
fn charge_readings(status: ChrgMgmtDataResp) -> Readings {
    let mut readings = Vec::new();
    let Some(b) = status.chrg_mgmt_data else {
        return readings;
    };
    if let Some(v @ 0..=1000) = b.bms_pack_soc_dsp {
        readings.push(("drivetrain/soc", json!(v as f64 / 10.)));
    }
    if let Some(v @ 1..=7) = b.bms_on_bd_chrg_trgt_soc_dsp_cmd {
        readings.push(("drivetrain/socTarget", json!(30 + v * 10)));
    }
    if let Some(v @ 0..=13) = b.bms_chrg_sts {
        readings.push(("drivetrain/charging", json!(matches!(v, 1 | 3 | 10 | 12))));
    }
    if let (Some(ac @ 0..=1), Some(dc @ 0..=1)) =
        (b.ccu_onbd_chrgr_plug_on, b.ccu_off_bd_chrgr_plug_on)
    {
        readings.push(("drivetrain/chargerConnected", json!(ac == 1 || dc == 1)));
    }
    if b.chrgng_rmnng_time_v == Some(0) {
        if let Some(v @ 0..=65535) = b.chrgng_rmnng_time {
            readings.push(("drivetrain/remainingChargingTime", json!(v * 60)));
        }
    }
    if b.bms_pack_crnt_v == Some(0) {
        if let (Some(current @ 0..=65535), Some(voltage @ 0..=65535)) =
            (b.bms_pack_crnt, b.bms_pack_vol)
        {
            let power = (current as f64 * 0.05 - 1000.) * voltage as f64 * 0.25 / 1000.;
            readings.push(("drivetrain/power", json!((power * 1000.).round() / 1000.)));
        }
    }
    readings
}

#[cfg(test)]
mod tests;
