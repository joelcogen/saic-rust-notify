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

#[derive(Default)]
struct Fake {
    calls: Arc<Mutex<Vec<String>>>,
    outcomes: VecDeque<Result<CommandState, ()>>,
    login_failures: usize,
    read_failure: bool,
    vehicle: VehicleStatusResp,
    charge: ChrgMgmtDataResp,
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
            vin_list: Some(vec![VinInfo {
                vin: Some("SYNTHETIC".into()),
                ..Default::default()
            }]),
            ..Default::default()
        })
    }
    async fn vehicle(&mut self, _: &str) -> Result<VehicleStatusResp, ()> {
        self.calls.lock().unwrap().push("vehicle".into());
        if self.read_failure {
            Err(())
        } else {
            Ok(self.vehicle.clone())
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
    let task = tokio::spawn(async move { worker(&mut fake, &config(), &shared, rx).await });
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
