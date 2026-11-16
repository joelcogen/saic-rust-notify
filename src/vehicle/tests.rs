use super::*;
use crate::telemetry::Telemetry;
use saic_client::{
    models::{BasicVehicleStatus, ChrgMgmtData, VinInfo},
    UnknownReason,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::RwLock;

struct Fake {
    calls: Arc<Mutex<Vec<String>>>,
    outcomes: VecDeque<Result<CommandState, ()>>,
    login_failures: usize,
    discovery_failures: usize,
    cached: VehicleStatusCacheResp,
    cache_calls: Arc<Mutex<usize>>,
    read_failure: bool,
    read_failures: usize,
    vehicle_calls: Arc<Mutex<Vec<String>>>,
    fleet: Vec<String>,
    active_vin: Option<String>,
    vehicle: VehicleStatusResp,
    charge: ChrgMgmtDataResp,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            calls: Default::default(),
            outcomes: Default::default(),
            login_failures: 0,
            discovery_failures: 0,
            cached: Default::default(),
            cache_calls: Default::default(),
            read_failure: false,
            read_failures: 0,
            vehicle_calls: Default::default(),
            fleet: vec![],
            active_vin: None,
            vehicle: VehicleStatusResp {
                status_time: Some(chrono::Utc::now().timestamp()),
                ..Default::default()
            },
            charge: Default::default(),
        }
    }
}
impl Session for Fake {
    async fn login(&mut self) -> Result<(), ()> {
        self.calls.lock().unwrap().push("login".into());
        if self.login_failures > 0 {
            self.login_failures -= 1;
            Err(())
        } else {
            Ok(())
        }
    }
    async fn vehicles(&mut self) -> Result<VehicleListResp, ()> {
        self.calls.lock().unwrap().push("discovery".into());
        if self.discovery_failures > 0 {
            self.discovery_failures -= 1;
            return Err(());
        }
        Ok(VehicleListResp {
            vin_list: Some(
                if self.fleet.is_empty() {
                    vec!["SYNTHETIC".into()]
                } else {
                    self.fleet.clone()
                }
                .into_iter()
                .map(|vin| VinInfo {
                    vin: Some(vin),
                    ..Default::default()
                })
                .collect(),
            ),
            ..Default::default()
        })
    }
    async fn cached(&mut self, _: &str) -> Result<VehicleStatusCacheResp, ()> {
        *self.cache_calls.lock().unwrap() += 1;
        Ok(self.cached.clone())
    }
    async fn vehicle(&mut self, vin: &str) -> Result<VehicleStatusResp, ()> {
        self.calls.lock().unwrap().push("vehicle".into());
        self.vehicle_calls.lock().unwrap().push(vin.into());
        if self.read_failure || self.read_failures > 0 {
            self.read_failures = self.read_failures.saturating_sub(1);
            Err(())
        } else {
            let mut vehicle = self.vehicle.clone();
            if self.active_vin.as_deref() == Some(vin) {
                vehicle.basic_vehicle_status = Some(BasicVehicleStatus {
                    engine_status: Some(1),
                    ..Default::default()
                });
            }
            Ok(vehicle)
        }
    }
    async fn charge(&mut self, _: &str) -> Result<ChrgMgmtDataResp, ()> {
        self.calls.lock().unwrap().push("charge".into());
        Ok(self.charge.clone())
    }
    async fn command(&mut self, _: &str, command: &Command) -> Result<CommandState, ()> {
        self.calls
            .lock()
            .unwrap()
            .push(serde_json::to_string(command).unwrap());
        self.outcomes
            .pop_front()
            .unwrap_or(Ok(CommandState::Completed))
    }
}
fn state() -> SharedTelemetry {
    Arc::new(RwLock::new(Telemetry::default()))
}
fn config() -> Config {
    Config::load(|key| {
        Ok(match key {
            "SAIC_USERNAME" => Some("synthetic".into()),
            "SAIC_PASSWORD" => Some("secret".into()),
            _ => None,
        })
    })
    .unwrap()
}

#[tokio::test]
async fn ordered_climate_commands_stop_on_failure_without_reads_or_replay() {
    for (failure, expected) in [
        (CommandState::Rejected, "partial"),
        (CommandState::InvalidSession, "unknown"),
        (CommandState::Unknown(UnknownReason::Timeout), "unknown"),
    ] {
        let mut fake = Fake {
            outcomes: VecDeque::from([Ok(CommandState::Completed), Ok(failure)]),
            ..Default::default()
        };
        assert!(
            execute(&mut fake, &state(), "SYNTHETIC", Action::Climate(2.))
                .await
                .unwrap_err()
                .contains(expected)
        );
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        let first: Value = serde_json::from_str(&calls[0]).unwrap();
        assert_eq!(first["rvcReqType"], "6");
        assert_eq!(
            first["rvcParams"][0],
            json!({"paramId":19,"paramValue":"BQ=="})
        );
        let second: Value = serde_json::from_str(&calls[1]).unwrap();
        assert_eq!(
            second["rvcParams"],
            json!([{"paramId":17,"paramValue":"Aw=="},{"paramId":255,"paramValue":"AAAAAA=="}])
        );
    }
    let mut fake = Fake::default();
    execute(&mut fake, &state(), "SYNTHETIC", Action::Climate(2.))
        .await
        .unwrap();
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.len(), 5);
    let requests: Vec<Value> = calls
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(requests[2]["rvcParams"][0]["paramId"], 18);
    assert_eq!(requests[3]["rvcReqType"], "8");
    assert_eq!(requests[4]["rvcReqType"], "32");
}

#[tokio::test]
async fn locate_is_lights_only_and_explicit_refresh_reads_both_sources() {
    let mut fake = Fake::default();
    execute(&mut fake, &state(), "SYNTHETIC", Action::Locate)
        .await
        .unwrap();
    assert_eq!(
        *fake.calls.lock().unwrap(),
        vec![serde_json::to_string(&Command::find_my_car(FindCarMode::LightsOnly)).unwrap()]
    );
    fake.calls.lock().unwrap().clear();
    execute(&mut fake, &state(), "SYNTHETIC", Action::Refresh)
        .await
        .unwrap();
    assert_eq!(*fake.calls.lock().unwrap(), vec!["vehicle", "charge"]);
}

#[tokio::test]
async fn direct_snapshot_uses_source_timestamp_and_never_revives_missing_fields() {
    let now = chrono::Utc::now();
    let mut fake = Fake {
        vehicle: VehicleStatusResp {
            status_time: Some(now.timestamp() - 601),
            basic_vehicle_status: Some(BasicVehicleStatus {
                interior_temperature: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        },
        charge: ChrgMgmtDataResp {
            chrg_mgmt_data: Some(ChrgMgmtData {
                bms_pack_soc_dsp: Some(755),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let state = state();
    refresh(&mut fake, &state, "SYNTHETIC").await.unwrap();
    let snapshot = state
        .read()
        .await
        .snapshot(Instant::now(), now, Duration::from_secs(600));
    assert_eq!(
        snapshot["readings"]["vehicle/climate/interiorTemperature"]["fresh"],
        false
    );
    assert_eq!(
        snapshot["readings"]["vehicle/drivetrain/soc"]["value"],
        75.5
    );
    assert_eq!(
        snapshot["readings"]["vehicle/drivetrain/soc"]["fresh"],
        true
    );
    fake.vehicle.status_time = Some(now.timestamp());
    refresh(&mut fake, &state, "SYNTHETIC").await.unwrap();
    assert!(state
        .read()
        .await
        .fresh(
            &Source::Vehicle("SYNTHETIC".into()),
            "vehicle/climate/interiorTemperature",
            Instant::now(),
            now,
            Duration::from_secs(600)
        )
        .is_some());
    fake.vehicle.basic_vehicle_status = None;
    refresh(&mut fake, &state, "SYNTHETIC").await.unwrap();
    assert!(state
        .read()
        .await
        .latest(
            &Source::Vehicle("SYNTHETIC".into()),
            "vehicle/climate/interiorTemperature"
        )
        .is_none());
    fake.read_failure = true;
    assert!(refresh(&mut fake, &state, "SYNTHETIC").await.is_err());
    assert!(state
        .read()
        .await
        .latest(
            &Source::Vehicle("SYNTHETIC".into()),
            "vehicle/drivetrain/soc"
        )
        .is_some());
}

#[test]
fn missing_unknown_and_invalid_measurements_do_not_become_off_or_zero() {
    let data = ChrgMgmtDataResp {
        chrg_mgmt_data: Some(ChrgMgmtData {
            bms_pack_soc_dsp: Some(1001),
            bms_chrg_sts: Some(99),
            bms_on_bd_chrg_trgt_soc_dsp_cmd: Some(99),
            bms_pack_crnt: Some(19000),
            bms_pack_vol: Some(1600),
            bms_pack_crnt_v: Some(1),
            chrgng_rmnng_time: Some(30),
            chrgng_rmnng_time_v: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(charge_readings(data).is_empty());
    let data = ChrgMgmtDataResp {
        chrg_mgmt_data: Some(ChrgMgmtData {
            bms_pack_soc_dsp: Some(755),
            bms_chrg_sts: Some(3),
            bms_on_bd_chrg_trgt_soc_dsp_cmd: Some(5),
            bms_pack_crnt: Some(19000),
            bms_pack_vol: Some(1600),
            bms_pack_crnt_v: Some(0),
            chrgng_rmnng_time: Some(30),
            chrgng_rmnng_time_v: Some(0),
            ccu_onbd_chrgr_plug_on: Some(1),
            ccu_off_bd_chrgr_plug_on: Some(0),
            ..Default::default()
        }),
        ..Default::default()
    };
    let readings: std::collections::BTreeMap<_, _> = charge_readings(data).into_iter().collect();
    assert_eq!(readings["drivetrain/soc"], 75.5);
    assert_eq!(readings["drivetrain/socTarget"], 80);
    assert_eq!(readings["drivetrain/power"], -20.);
    assert_eq!(readings["drivetrain/charging"], true);
    assert_eq!(readings["drivetrain/chargerConnected"], true);
    assert_eq!(readings["drivetrain/remainingChargingTime"], 1800);
    let readings = vehicle_readings(VehicleStatusResp {
        basic_vehicle_status: Some(BasicVehicleStatus {
            interior_temperature: Some(87),
            exterior_temperature: Some(12),
            remote_climate_status: Some(99),
            lock_status: Some(99),
            ..Default::default()
        }),
        ..Default::default()
    });
    assert!(readings.is_empty());
}

#[test]
fn discovery_is_bounded_and_configured_vin_must_belong_to_account() {
    let list = VehicleListResp {
        vin_list: Some(vec![
            VinInfo {
                vin: Some("ONE".into()),
                ..Default::default()
            },
            VinInfo {
                vin: Some("TWO".into()),
                ..Default::default()
            },
        ]),
        ..Default::default()
    };
    assert_eq!(
        select_vehicles(list.clone(), None).unwrap(),
        vec!["ONE", "TWO"]
    );
    assert_eq!(
        select_vehicles(list.clone(), Some("TWO")).unwrap(),
        vec!["TWO"]
    );
    assert!(select_vehicles(list, Some("OTHER")).is_err());
    assert!(select_vehicles(VehicleListResp::default(), None).is_err());
    assert!(select_vehicles(
        VehicleListResp {
            vin_list: Some(vec![VinInfo::default()]),
            ..Default::default()
        },
        None
    )
    .is_err());
}

#[tokio::test(start_paused = true)]
async fn worker_retries_login_polls_and_never_replays_uncertain_commands() {
    let mut fake = Fake {
        login_failures: 1,
        outcomes: VecDeque::from([Err(())]),
        ..Default::default()
    };
    let calls = fake.calls.clone();
    let state = state();
    let shared = state.clone();
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        worker(&mut fake, &config(), &shared, rx, &mut Schedules::default()).await
    });
    tokio::task::yield_now().await;
    assert_eq!(*calls.lock().unwrap(), vec!["login"]);
    assert!(!state.read().await.is_connected());
    tokio::time::advance(Duration::from_secs(60)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["login", "login", "discovery", "vehicle", "charge"]
    );
    assert!(state.read().await.is_connected());
    let (reply, receive) = oneshot::channel();
    tx.send(Request {
        vin: "SYNTHETIC".into(),
        action: Action::Locate,
        reply,
    })
    .await
    .unwrap();
    assert!(receive.await.unwrap().unwrap_err().contains("unknown"));
    settle().await;
    // Reauthentication restores readiness before the next scheduled read.
    assert!(state.read().await.control_base().is_ok());
    let snapshot =
        state
            .read()
            .await
            .snapshot(Instant::now(), chrono::Utc::now(), Duration::from_secs(600));
    assert_eq!(
        snapshot["readings"]["vehicle/refresh/lastVehicleState"]["fresh"],
        false
    );
    tokio::time::advance(Duration::from_secs(300)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with('{'))
            .count(),
        1
    );
    assert!(state.read().await.is_connected());
    let (reply, receive) = oneshot::channel();
    drop(receive);
    tx.send(Request {
        vin: "SYNTHETIC".into(),
        action: Action::Locate,
        reply,
    })
    .await
    .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with('{'))
            .count(),
        1
    );
    drop(tx);
    task.await.unwrap();
}

#[test]
fn invalid_api_configuration_is_redacted_without_network_access() {
    let mut config = config();
    config.api_base_url = Some("http://secret-user:secret-password@example.invalid/".into());
    let error = client(&config).err().unwrap();
    assert!(error.contains("SAIC_BASE_URL"));
    assert!(!error.contains("secret"));
    config.api_base_url = Some("https://127.0.0.1:9/".into());
    assert!(client(&config).is_ok());
}

async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn worker_vehicles_have_independent_deadlines_and_never_catch_up() {
    let mut fake = Fake {
        fleet: vec!["PARKED".into(), "ACTIVE".into()],
        active_vin: Some("ACTIVE".into()),
        ..Default::default()
    };
    let calls = fake.vehicle_calls.clone();
    let shared = state();
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        worker(&mut fake, &config(), &shared, rx, &mut Schedules::default()).await;
    });
    settle().await;
    assert_eq!(*calls.lock().unwrap(), vec!["PARKED", "ACTIVE"]);
    tokio::time::advance(Duration::from_secs(30)).await;
    settle().await;
    assert_eq!(*calls.lock().unwrap(), vec!["PARKED", "ACTIVE", "ACTIVE"]);
    tokio::time::advance(Duration::from_secs(3 * 86400)).await;
    settle().await;
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["PARKED", "ACTIVE", "ACTIVE", "PARKED", "ACTIVE"]
    );
    settle().await;
    assert_eq!(calls.lock().unwrap().len(), 5);
    drop(tx);
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn worker_refresh_and_control_reconnect_preserve_known_parking_age() {
    let now = chrono::Utc::now().timestamp();
    let epoch = now - schedule::WINDOW;
    let mut schedules = Schedules::default();
    schedules
        .vehicle("SYNTHETIC")
        .success(epoch, Observation::default());
    schedules
        .vehicle("SYNTHETIC")
        .success(now, Observation::default());
    let mut fake = Fake {
        outcomes: VecDeque::from([Err(())]),
        ..Default::default()
    };
    let calls = fake.calls.clone();
    let shared = state();
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        worker(&mut fake, &config(), &shared, rx, &mut schedules).await;
        schedules
    });
    settle().await;
    assert_eq!(*calls.lock().unwrap(), vec!["login", "discovery"]);
    for action in [Action::Refresh, Action::Locate] {
        let (reply, receive) = oneshot::channel();
        tx.send(Request {
            vin: "SYNTHETIC".into(),
            action,
            reply,
        })
        .await
        .unwrap();
        let _ = receive.await.unwrap();
        settle().await;
    }
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.as_str() == "vehicle")
            .count(),
        1
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.as_str() == "login")
            .count(),
        2
    );
    tokio::time::advance(Duration::from_secs(86400 - 1)).await;
    settle().await;
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.as_str() == "vehicle")
            .count(),
        1
    );
    drop(tx);
    let mut schedules = task.await.unwrap();
    assert_eq!(schedules.vehicle("SYNTHETIC").parked_since, Some(epoch));
    assert_eq!(schedules.vehicle("SYNTHETIC").due(), now + schedule::DAY);
}

#[tokio::test]
async fn another_vehicles_success_never_revives_failed_readings() {
    let shared = state();
    let mut fake = Fake {
        charge: ChrgMgmtDataResp {
            chrg_mgmt_data: Some(ChrgMgmtData {
                bms_pack_soc_dsp: Some(500),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut schedules = Schedules::default();
    poll_vehicle(&mut fake, &shared, "ONE", &mut schedules, &|| 1000)
        .await
        .unwrap();
    fake.read_failure = true;
    assert!(
        poll_vehicle(&mut fake, &shared, "ONE", &mut schedules, &|| 1001)
            .await
            .is_err()
    );
    fake.read_failure = false;
    poll_vehicle(&mut fake, &shared, "TWO", &mut schedules, &|| 1002)
        .await
        .unwrap();
    let state = shared.read().await;
    let fresh = |vin: &str| {
        state
            .fresh(
                &Source::Vehicle(vin.into()),
                "vehicle/drivetrain/soc",
                Instant::now(),
                chrono::Utc::now(),
                Duration::from_secs(600),
            )
            .is_some()
    };
    assert!(!fresh("ONE"));
    assert!(fresh("TWO"));
}

#[tokio::test(start_paused = true)]
async fn worker_reauthentication_does_not_reset_exponential_backoff() {
    let mut fake = Fake {
        read_failures: 2,
        ..Default::default()
    };
    let calls = fake.vehicle_calls.clone();
    let shared = state();
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        worker(&mut fake, &config(), &shared, rx, &mut Schedules::default()).await;
    });
    settle().await;
    assert_eq!(calls.lock().unwrap().len(), 1);
    tokio::time::advance(Duration::from_secs(29)).await;
    settle().await;
    assert_eq!(calls.lock().unwrap().len(), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(calls.lock().unwrap().len(), 2);
    tokio::time::advance(Duration::from_secs(59)).await;
    settle().await;
    assert_eq!(calls.lock().unwrap().len(), 2);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(calls.lock().unwrap().len(), 3);
    tokio::time::advance(Duration::from_secs(119)).await;
    settle().await;
    assert_eq!(calls.lock().unwrap().len(), 3);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(calls.lock().unwrap().len(), 4);
    drop(tx);
    task.await.unwrap();
}

#[test]
fn vehicle_timestamp_validation_matches_gateway_boundaries() {
    let now = 1_800_000_000;
    for stamp in [
        None,
        Some(0),
        Some(2_147_483_647),
        Some(i64::MIN),
        Some(i64::MAX),
        Some(now - 901),
        Some(now + 901),
    ] {
        assert!(!valid_vehicle_timestamp(stamp, now));
    }
    for stamp in [now - 900, now, now + 900] {
        assert!(valid_vehicle_timestamp(Some(stamp), now));
    }
    assert!(!valid_vehicle_timestamp(Some(2_147_483_647), 2_147_483_647));
}

#[tokio::test]
async fn stale_active_responses_back_off_without_resetting_parking_or_telemetry() {
    let now = chrono::Utc::now().timestamp();
    let state = state();
    let mut schedules = Schedules::default();
    let mut fake = Fake::default();
    fake.vehicle.basic_vehicle_status = Some(BasicVehicleStatus {
        interior_temperature: Some(5),
        ..Default::default()
    });
    poll_vehicle(&mut fake, &state, "SYNTHETIC", &mut schedules, &|| now)
        .await
        .unwrap();
    let epoch = schedules.vehicle("SYNTHETIC").parked_since;
    fake.active_vin = Some("SYNTHETIC".into());
    for (index, stamp) in [Some(now - 901), None, Some(0), Some(now + 3600)]
        .into_iter()
        .enumerate()
    {
        fake.vehicle.status_time = stamp;
        fake.calls.lock().unwrap().clear();
        assert!(
            poll_vehicle(&mut fake, &state, "SYNTHETIC", &mut schedules, &|| now)
                .await
                .is_err()
        );
        assert_eq!(schedules.vehicle("SYNTHETIC").parked_since, epoch);
        assert_eq!(
            schedules.vehicle("SYNTHETIC").due(),
            now + 30 * (1 << index)
        );
        assert_eq!(*fake.calls.lock().unwrap(), vec!["vehicle"]);
        let snapshot = state.read().await.snapshot(
            Instant::now(),
            chrono::Utc::now(),
            Duration::from_secs(600),
        );
        assert_eq!(
            snapshot["readings"]["vehicle/climate/interiorTemperature"]["value"],
            5
        );
        assert_eq!(
            snapshot["readings"]["vehicle/climate/interiorTemperature"]["fresh"],
            false
        );
    }
    fake.vehicle.status_time = Some(now);
    poll_vehicle(&mut fake, &state, "SYNTHETIC", &mut schedules, &|| now)
        .await
        .unwrap();
    assert_eq!(schedules.vehicle("SYNTHETIC").parked_since, None);
    assert_eq!(schedules.vehicle("SYNTHETIC").due(), now + 30);
    schedules.vehicle("SYNTHETIC").failure(now);
    assert_eq!(schedules.vehicle("SYNTHETIC").due(), now + 30);
}

async fn http_request(
    controls: &crate::controls::Controls,
    method: &str,
    path: &str,
    authorized: bool,
) -> (u16, String) {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    let mut request = Request::builder().method(method).uri(path);
    if authorized {
        request = request.header("authorization", "Bearer synthetic-token");
    }
    let response = crate::http::router(controls.clone(), Some("synthetic-token"))
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    (
        response.status().as_u16(),
        String::from_utf8(
            to_bytes(response.into_body(), 100_000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap(),
    )
}

#[tokio::test(start_paused = true)]
async fn restart_seeds_cache_without_live_reads_before_saved_deadline() {
    let now = chrono::Utc::now().timestamp();
    let parked = now - schedule::WINDOW;
    let path = std::env::temp_dir().join(format!("refresh-readiness-{}.json", std::process::id()));
    let mut saved = Schedules::open(path.clone()).unwrap();
    saved
        .vehicle("SYNTHETIC")
        .success(parked, Observation::default());
    saved
        .vehicle("SYNTHETIC")
        .success(now, Observation::default());
    saved.save().unwrap();
    drop(saved);
    let mut schedules = Schedules::open(path.clone()).unwrap();
    let deadline = schedules.vehicle("SYNTHETIC").due();
    assert_eq!(deadline, now + schedule::DAY);
    let mut fake = Fake {
        cached: VehicleStatusCacheResp {
            status_time: Some(now - 86400),
            ..Default::default()
        },
        ..Default::default()
    };
    let cache_calls = fake.cache_calls.clone();
    let calls = fake.calls.clone();
    let shared = state();
    let (controls, rx) = crate::controls::Controls::new(shared.clone(), Duration::from_secs(600));
    let task = tokio::spawn(async move {
        worker(&mut fake, &config(), &shared, rx, &mut schedules).await;
        schedules
    });
    settle().await;
    tokio::time::advance(Duration::from_secs(60)).await;
    settle().await;
    let (code, body) = http_request(&controls, "GET", "/api/status", true).await;
    assert_eq!(code, 200);
    let status: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status["connected"], true);
    assert_eq!(status["ready"], true);
    assert_eq!(status["sources"].as_array().unwrap().len(), 1);
    assert_eq!(*cache_calls.lock().unwrap(), 1);
    assert_eq!(
        status["readings"]["vehicle/refresh/lastVehicleState"]["fresh"],
        false
    );
    assert_eq!(
        status["readings"]["vehicle/refresh/lastVehicleState"]["value"],
        chrono::DateTime::from_timestamp(now - 86400, 0)
            .unwrap()
            .to_rfc3339()
    );
    assert_eq!(
        http_request(&controls, "POST", "/refresh", false).await.0,
        401
    );
    assert_eq!(
        controls.execute("/climate/start", None).await,
        Err("Fresh interior temperature is unavailable")
    );
    assert_eq!(*calls.lock().unwrap(), vec!["login", "discovery"]);
    assert_eq!(
        http_request(&controls, "POST", "/refresh", true).await,
        (202, "Vehicle refreshed".into())
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["login", "discovery", "vehicle", "charge"]
    );
    drop(controls);
    let mut schedules = task.await.unwrap();
    assert_eq!(schedules.vehicle("SYNTHETIC").parked_since, Some(parked));
    assert_eq!(schedules.vehicle("SYNTHETIC").due(), deadline);
    let mut persisted = Schedules::open(path.clone()).unwrap();
    assert_eq!(persisted.vehicle("SYNTHETIC").parked_since, Some(parked));
    assert_eq!(persisted.vehicle("SYNTHETIC").due(), deadline);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test(start_paused = true)]
async fn discovery_and_login_failures_deny_refresh_until_reauthenticated() {
    for scenario in ["login", "discovery", "membership", "multiple"] {
        let mut fake = Fake::default();
        let mut config = config();
        match scenario {
            "login" => fake.login_failures = 1,
            "discovery" => fake.discovery_failures = 1,
            "membership" => config.vin = Some("OTHER".into()),
            _ => fake.fleet = vec!["ONE".into(), "TWO".into()],
        }
        let mut schedules = Schedules::default();
        for vin in ["SYNTHETIC", "ONE", "TWO"] {
            schedules.vehicle(vin).next = chrono::Utc::now().timestamp() + 86400;
        }
        let calls = fake.vehicle_calls.clone();
        let shared = state();
        let (controls, rx) =
            crate::controls::Controls::new(shared.clone(), Duration::from_secs(600));
        let task = tokio::spawn(async move {
            worker(&mut fake, &config, &shared, rx, &mut schedules).await;
        });
        settle().await;
        let status: Value =
            serde_json::from_str(&http_request(&controls, "GET", "/api/status", true).await.1)
                .unwrap();
        assert_eq!(status["ready"], false);
        let error = controls.execute("/refresh", None).await.unwrap_err();
        assert_eq!(
            error,
            if scenario == "multiple" {
                "A single authorized vehicle is required"
            } else {
                "SAIC session or vehicle discovery unavailable"
            }
        );
        assert!(calls.lock().unwrap().is_empty());
        if matches!(scenario, "login" | "discovery") {
            tokio::time::advance(Duration::from_secs(60)).await;
            settle().await;
            assert!(controls.state.read().await.control_base().is_ok());
            assert!(calls.lock().unwrap().is_empty());
            assert_eq!(
                controls.execute("/refresh", None).await,
                Ok("Vehicle refreshed")
            );
        }
        task.abort();
    }
}

#[tokio::test(start_paused = true)]
async fn failed_explicit_refresh_restores_readiness_but_not_freshness_after_relogin() {
    for stale in [false, true] {
        let now = chrono::Utc::now().timestamp();
        let mut schedules = Schedules::default();
        let parked = now - schedule::WINDOW;
        schedules
            .vehicle("SYNTHETIC")
            .success(parked, Observation::default());
        schedules
            .vehicle("SYNTHETIC")
            .success(now, Observation::default());
        let shared = state();
        let mut fake = Fake::default();
        fake.vehicle.basic_vehicle_status = Some(BasicVehicleStatus {
            interior_temperature: Some(5),
            ..Default::default()
        });
        refresh(&mut fake, &shared, "SYNTHETIC").await.unwrap();
        fake.calls.lock().unwrap().clear();
        if stale {
            fake.vehicle.status_time = Some(now - 901);
        } else {
            fake.read_failure = true;
        }
        let calls = fake.calls.clone();
        let (controls, rx) =
            crate::controls::Controls::new(shared.clone(), Duration::from_secs(600));
        let task = tokio::spawn(async move {
            worker(&mut fake, &config(), &shared, rx, &mut schedules).await;
            schedules
        });
        settle().await;
        for _ in 0..2 {
            assert_eq!(
                controls.execute("/refresh", None).await,
                Err("Vehicle refresh failed")
            );
            settle().await;
            let state = controls.state.read().await;
            assert!(state.control_base().is_ok());
            let snapshot =
                state.snapshot(Instant::now(), chrono::Utc::now(), Duration::from_secs(600));
            assert_eq!(
                snapshot["readings"]["vehicle/climate/interiorTemperature"]["value"],
                5
            );
            assert_eq!(
                snapshot["readings"]["vehicle/climate/interiorTemperature"]["fresh"],
                false
            );
            drop(state);
            assert_eq!(
                controls.execute("/climate/start", None).await,
                Err("Fresh interior temperature is unavailable")
            );
        }
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "login",
                "discovery",
                "vehicle",
                "login",
                "discovery",
                "vehicle",
                "login",
                "discovery"
            ]
        );
        drop(controls);
        let mut schedules = task.await.unwrap();
        assert_eq!(schedules.vehicle("SYNTHETIC").parked_since, Some(parked));
        assert_eq!(schedules.vehicle("SYNTHETIC").due(), now + 60);
    }
}

#[tokio::test(start_paused = true)]
async fn climate_batch_refreshes_once_and_resumes_polling_without_replaying_commands() {
    for temperature in [2., 5., 10.] {
        for read_failure in [false, true] {
            let now = chrono::Utc::now().timestamp();
            let mut schedules = Schedules::default();
            schedules
                .vehicle("SYNTHETIC")
                .success(now - schedule::WINDOW, Observation::default());
            schedules
                .vehicle("SYNTHETIC")
                .success(now, Observation::default());
            let mut fake = Fake {
                read_failure,
                vehicle: VehicleStatusResp {
                    status_time: Some(now),
                    basic_vehicle_status: Some(BasicVehicleStatus {
                        remote_climate_status: Some(2),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            };
            let calls = fake.calls.clone();
            let shared = state();
            let worker_state = shared.clone();
            let (tx, rx) = mpsc::channel(16);
            let task = tokio::spawn(async move {
                worker(&mut fake, &config(), &worker_state, rx, &mut schedules).await;
                schedules
            });
            settle().await;
            let (reply, receive) = oneshot::channel();
            tx.send(Request {
                vin: "SYNTHETIC".into(),
                action: Action::Climate(temperature),
                reply,
            })
            .await
            .unwrap();
            assert_eq!(receive.await.unwrap(), Ok(climate_plan(temperature).1));
            settle().await;
            let mut expected = vec!["login".to_string(), "discovery".to_string()];
            expected.extend(
                climate_plan(temperature)
                    .0
                    .iter()
                    .map(|c| serde_json::to_string(c).unwrap()),
            );
            expected.push("vehicle".into());
            if read_failure {
                expected.extend(["login".into(), "discovery".into()]);
            } else {
                expected.push("charge".into());
                let snapshot = shared.read().await.snapshot(
                    Instant::now(),
                    chrono::Utc::now(),
                    Duration::from_secs(600),
                );
                assert_eq!(
                    snapshot["readings"]["vehicle/climate/remoteClimateState"]["value"],
                    "on"
                );
                assert_eq!(
                    snapshot["readings"]["vehicle/climate/remoteClimateState"]["fresh"],
                    true
                );
            }
            assert_eq!(*calls.lock().unwrap(), expected);
            tokio::time::advance(Duration::from_secs(29)).await;
            settle().await;
            assert_eq!(*calls.lock().unwrap(), expected);
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;
            expected.push("vehicle".into());
            if read_failure {
                expected.extend(["login".into(), "discovery".into()]);
            } else {
                expected.push("charge".into());
            }
            assert_eq!(*calls.lock().unwrap(), expected);
            drop(tx);
            task.await.unwrap();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn failed_climate_batch_does_not_refresh_or_change_deadline() {
    for failed_step in 0..5 {
        let now = chrono::Utc::now().timestamp();
        let mut schedules = Schedules::default();
        schedules
            .vehicle("SYNTHETIC")
            .success(now, Observation::default());
        let due = schedules.vehicle("SYNTHETIC").due();
        let mut outcomes: VecDeque<_> = (0..failed_step)
            .map(|_| Ok(CommandState::Completed))
            .collect();
        outcomes.push_back(Ok(CommandState::Rejected));
        let mut fake = Fake {
            outcomes,
            ..Default::default()
        };
        let calls = fake.calls.clone();
        let shared = state();
        let (tx, rx) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            worker(&mut fake, &config(), &shared, rx, &mut schedules).await;
            schedules
        });
        settle().await;
        let (reply, receive) = oneshot::channel();
        tx.send(Request {
            vin: "SYNTHETIC".into(),
            action: Action::Climate(2.),
            reply,
        })
        .await
        .unwrap();
        assert!(receive.await.unwrap().is_err());
        settle().await;
        drop(tx);
        let mut schedules = task.await.unwrap();
        assert_eq!(schedules.vehicle("SYNTHETIC").due(), due);
        let mut expected = vec!["login".to_string(), "discovery".to_string()];
        expected.extend(
            climate_plan(2.)
                .0
                .iter()
                .take(failed_step + 1)
                .map(|c| serde_json::to_string(c).unwrap()),
        );
        expected.extend(["login".into(), "discovery".into()]);
        assert_eq!(*calls.lock().unwrap(), expected);
    }
}

#[test]
fn dashboard_fields_decode_without_inventing_missing_states() {
    let readings: std::collections::BTreeMap<_, _> = vehicle_readings(VehicleStatusResp {
        basic_vehicle_status: Some(BasicVehicleStatus {
            driver_door: Some(1),
            passenger_door: Some(0),
            rear_left_door: Some(0),
            rear_right_door: Some(1),
            bonnet_status: Some(0),
            boot_status: Some(1),
            mileage: Some(12345),
            driver_window: Some(1),
            passenger_window: Some(0),
            rear_left_window: Some(0),
            rear_right_window: Some(1),
            front_left_tyre_pressure: Some(65),
            front_right_tyre_pressure: Some(64),
            rear_left_tyre_pressure: Some(255),
            rear_right_tyre_pressure: Some(0),
            second_row_left_seat_heat_level: Some(1),
            second_row_right_seat_heat_level: Some(0),
            steering_heat_level: Some(1),
            engine_status: Some(1),
            hand_brake: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    })
    .into_iter()
    .collect();
    assert_eq!(readings["tyres/frontLeftPressure"], 2.6);
    assert_eq!(readings["tyres/frontRightPressure"], 2.56);
    assert!(!readings.contains_key("tyres/rearLeftPressure"));
    assert!(!readings.contains_key("tyres/rearRightPressure"));
    for key in [
        "doors/driver",
        "doors/rearRight",
        "doors/boot",
        "windows/driver",
        "windows/rearRight",
        "climate/steeringWheelHeating",
        "drivetrain/parked",
    ] {
        assert_eq!(readings[key], true, "{key}");
    }
    for key in [
        "doors/passenger",
        "doors/rearLeft",
        "doors/bonnet",
        "windows/passenger",
        "windows/rearLeft",
    ] {
        assert_eq!(readings[key], false, "{key}");
    }
    assert_eq!(readings["climate/heatedSeatsRearLeftLevel"], 1);
    assert_eq!(readings["climate/heatedSeatsRearRightLevel"], 0);
    let unknown = vehicle_readings(VehicleStatusResp {
        basic_vehicle_status: Some(BasicVehicleStatus {
            driver_door: Some(2),
            bonnet_status: Some(255),
            boot_status: Some(-1),
            driver_window: Some(0), // incomplete status: no valid mileage
            second_row_left_seat_heat_level: Some(255),
            steering_heat_level: Some(99),
            front_left_tyre_pressure: Some(-1),
            engine_status: Some(99),
            ..Default::default()
        }),
        ..Default::default()
    });
    assert!(unknown.is_empty());
    for (engine, brake, parked) in [
        (Some(0), None, Some(true)),
        (Some(1), Some(0), Some(false)),
        (Some(1), None, None),
    ] {
        let data: std::collections::BTreeMap<_, _> = vehicle_readings(VehicleStatusResp {
            basic_vehicle_status: Some(BasicVehicleStatus {
                engine_status: engine,
                hand_brake: brake,
                ..Default::default()
            }),
            ..Default::default()
        })
        .into_iter()
        .collect();
        assert_eq!(
            data.get("drivetrain/parked"),
            parked.map(serde_json::Value::Bool).as_ref()
        );
    }
}

#[tokio::test]
async fn cached_snapshot_keeps_old_timestamp_and_internal_freshness() {
    let shared = state();
    shared.write().await.set_connected(true);
    let stamp = chrono::Utc::now().timestamp() - 86400;
    seed_cached(
        &shared,
        "SYNTHETIC",
        VehicleStatusCacheResp {
            status_time: Some(stamp),
            basic_vehicle_status: Some(BasicVehicleStatus {
                engine_status: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await;
    let snapshot =
        shared
            .read()
            .await
            .snapshot(Instant::now(), chrono::Utc::now(), Duration::from_secs(600));
    let reading = &snapshot["readings"]["vehicle/refresh/lastVehicleState"];
    assert_eq!(
        reading["value"],
        chrono::DateTime::from_timestamp(stamp, 0)
            .unwrap()
            .to_rfc3339()
    );
    assert_eq!(reading["fresh"], false);
    seed_cached(
        &shared,
        "SYNTHETIC",
        VehicleStatusCacheResp {
            status_time: Some(stamp - 3600),
            ..Default::default()
        },
    )
    .await;
    let after =
        shared
            .read()
            .await
            .snapshot(Instant::now(), chrono::Utc::now(), Duration::from_secs(600));
    assert_eq!(snapshot["readings"], after["readings"]);
}
