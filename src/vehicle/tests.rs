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
async fn locate_is_lights_only_and_refresh_is_the_only_action_that_reads() {
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
            exterior_temperature: Some(128),
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
    assert!(!state.read().await.is_connected());
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
