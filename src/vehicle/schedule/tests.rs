use super::*;
use saic_client::models::{BasicVehicleStatus, ChrgMgmtData, RvsChargeStatus};

fn parked() -> Observation {
    Observation::default()
}
fn active() -> Observation {
    Observation {
        active: true,
        ..parked()
    }
}
fn charging(power: f64) -> Observation {
    Observation {
        active: true,
        charging: true,
        power: Some(power),
        capacity: Some(64.),
        ..parked()
    }
}

#[test]
fn shutdown_and_staged_parking_include_the_48_hour_poll() {
    let mut s = Schedule::default();
    s.success(1000, active());
    assert_eq!(s.due(), 1030);
    s.success(1030, parked());
    let epoch = 1030;
    assert_eq!(s.parked_since, Some(epoch));
    for age in [120, 240, 360, 480] {
        assert_eq!(s.due(), epoch + age);
        s.success(epoch + age, parked());
    }
    for age in [
        HALF_DAY,
        DAY,
        DAY + HALF_DAY,
        WINDOW,
        WINDOW + DAY,
        WINDOW + 2 * DAY,
    ] {
        assert_eq!(s.due(), epoch + age);
        s.success(epoch + age, parked());
        assert_eq!(s.parked_since, Some(epoch));
    }
    s.success(epoch + 4 * DAY, active());
    assert_eq!(s.parked_since, None);
    s.success(epoch + 4 * DAY + ACTIVE, parked());
    assert_eq!(s.parked_since, Some(epoch + 4 * DAY + ACTIVE));
}

#[test]
fn manual_reads_do_not_move_epoch_and_delays_skip_missed_ticks() {
    let mut s = Schedule::default();
    s.success(0, parked());
    s.success(350, parked());
    assert_eq!(s.parked_since, Some(0));
    assert_eq!(s.due(), 470);
    s.success(HALF_DAY - 1, parked());
    assert_eq!(s.due(), HALF_DAY);
    s.success(WINDOW + 2 * DAY + 1, parked());
    assert_eq!(s.due(), WINDOW + 3 * DAY);
    assert_eq!(s.parked_since, Some(0));
}

#[test]
fn alarms_coalesce_and_respect_recent_reads_charging_and_backoff() {
    let mut s = Schedule::default();
    s.success(0, parked());
    s.success(700, parked());
    s.note_alarm(710);
    s.note_alarm(720);
    assert_eq!(s.due(), 760);
    s.success(730, parked()); // An intervening manual read satisfies both alerts.
    assert_eq!(s.due(), HALF_DAY);
    assert_eq!(s.parked_since, Some(0));

    s.note_alarm(1000);
    assert_eq!(s.due(), 1000);
    s.success(1000, charging(-7.));
    let charging_due = s.due();
    s.note_alarm(1100);
    assert_eq!(s.due(), charging_due);

    for now in [charging_due, charging_due + 30, charging_due + 90] {
        s.failure(now);
    }
    let retry = s.due();
    s.note_alarm(retry - 30);
    assert_eq!(s.due(), retry);
}

#[test]
fn charging_precedes_active_and_has_dynamic_bounds_and_trickle_fallback() {
    let mut s = Schedule::default();
    s.success(0, charging(-10.));
    assert_eq!(s.due(), 230); // 36 * 64 / 10, gateway rounded interval
    s.success(230, charging(-0.5));
    assert_eq!(s.due(), 460); // retain a previously longer interval
    let mut o = charging(-10.);
    o.remaining = Some(60);
    s.success(500, o);
    assert_eq!(s.due(), 560);
    s.success(560, charging(-0.5));
    assert_eq!(s.due(), 680);
    s.success(700, charging(-1000.));
    assert_eq!(s.due(), 730);
    let mut o = charging(-2.);
    o.capacity = Some(100000.);
    s.success(800, o);
    assert_eq!(s.due(), 800 + DAY);
    s.success(900, active());
    s.success(930, charging(0.));
    assert_eq!(s.due(), 1050);
    assert_eq!(s.parked_since, None);
}

#[test]
fn recovery_has_precedence_backoff_caps_and_resets_on_success() {
    let mut s = Schedule::default();
    s.success(0, parked());
    for exponent in 0..16 {
        let now = exponent;
        s.failure(now);
        let delay = (ACTIVE * (1 << exponent)).min(HALF_DAY);
        assert_eq!(s.due(), now + delay);
        // Repeated explicit failures, still in first 48 hours.
    }
    s.failure(WINDOW);
    assert_eq!(s.due(), WINDOW + DAY);
    s.last_activity = Some(WINDOW + 1);
    assert_eq!(s.due(), WINDOW + 1); // new activity bypasses recovery
    s.success(WINDOW + 1, charging(-10.));
    s.failure(WINDOW + 2);
    assert_eq!(s.due(), WINDOW + 2 + ACTIVE); // success reset exponent
    s.failure(WINDOW + 3);
    assert_eq!(s.due(), WINDOW + 3 + ACTIVE * 2);
}

#[test]
fn restart_and_rediscovery_preserve_age_deadline_and_failure_count() {
    let path = std::env::temp_dir()
        .join(format!(
            "saic-schedule-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ))
        .join("polling.json");
    let mut store = Schedules::open(path.clone()).unwrap();
    store.vehicle("SYNTHETIC-ONE").success(1000, parked());
    store.vehicle("SYNTHETIC-TWO").success(1000, active());
    store
        .vehicle("SYNTHETIC-ONE")
        .success(1000 + WINDOW, parked());
    store.vehicle("SYNTHETIC-TWO").failure(1030);
    store.save().unwrap();
    assert!(!fs::read_to_string(&path).unwrap().contains("SYNTHETIC"));
    drop(store);
    let mut store = Schedules::open(path.clone()).unwrap();
    assert_eq!(store.vehicle("SYNTHETIC-ONE").parked_since, Some(1000));
    assert_eq!(store.vehicle("SYNTHETIC-ONE").due(), 1000 + WINDOW + DAY);
    assert_eq!(store.vehicle("SYNTHETIC-TWO").due(), 1060);
    store.vehicle("SYNTHETIC-TWO").failure(1060);
    assert_eq!(store.vehicle("SYNTHETIC-TWO").due(), 1120);
    store
        .vehicle("SYNTHETIC-ONE")
        .success(1000 + WINDOW + 1, parked());
    assert_eq!(store.vehicle("SYNTHETIC-ONE").due(), 1000 + WINDOW + DAY);
    fs::write(&path, "broken").unwrap();
    assert!(Schedules::open(path.clone()).is_err());
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn typed_detection_matches_gateway_and_scheduled_charge_grace() {
    let now = Local
        .with_ymd_and_hms(2026, 1, 15, 1, 0, 0)
        .unwrap()
        .timestamp();
    for b in [
        BasicVehicleStatus {
            engine_status: Some(1),
            ..Default::default()
        },
        BasicVehicleStatus {
            remote_climate_status: Some(1),
            ..Default::default()
        },
        BasicVehicleStatus {
            rmt_htd_rr_wnd_st: Some(1),
            ..Default::default()
        },
    ] {
        assert!(
            Observation::read(
                &VehicleStatusResp {
                    basic_vehicle_status: Some(b),
                    ..Default::default()
                },
                &ChrgMgmtDataResp::default(),
                now
            )
            .active
        );
    }
    for mode in [1, 2, 3, 99] {
        let charge = ChrgMgmtDataResp {
            chrg_mgmt_data: Some(ChrgMgmtData {
                bms_chrg_sts: Some(3),
                bms_pack_crnt_v: Some(0),
                bms_pack_crnt: Some(19000),
                bms_pack_vol: Some(1600),
                chrgng_rmnng_time_v: Some(0),
                chrgng_rmnng_time: Some(20),
                bms_reser_ctrl_dsp_cmd: Some(mode),
                bms_reser_st_hour_dsp_cmd: Some(2),
                bms_reser_st_mintue_dsp_cmd: Some(0),
                ..Default::default()
            }),
            rvs_charge_status: Some(RvsChargeStatus {
                total_battery_capacity: Some(640),
                ..Default::default()
            }),
            ..Default::default()
        };
        let o = Observation::read(&VehicleStatusResp::default(), &charge, now);
        assert!(o.charging);
        assert_eq!(o.power, Some(-20.));
        assert_eq!(o.capacity, Some(64.));
        assert_eq!(o.remaining, Some(1200));
        assert_eq!(
            o.scheduled,
            matches!(mode, 1 | 3).then_some(now + 3600 + GRACE)
        );
    }
    let mut s = Schedule::default();
    s.success(
        now,
        Observation {
            scheduled: Some(now + 3600 + GRACE),
            ..parked()
        },
    );
    s.success(
        now + GRACE,
        Observation {
            scheduled: Some(now + 3600 + GRACE),
            ..parked()
        },
    );
    assert_eq!(s.due(), now + 3600 + GRACE);
    s.failure(s.due());
    assert_eq!(s.due(), now + 3600 + GRACE + ACTIVE);
}

#[test]
fn scheduled_grace_crosses_midnight_without_skipping_todays_check() {
    let now = Local
        .with_ymd_and_hms(2026, 1, 15, 0, 5, 0)
        .unwrap()
        .timestamp();
    assert_eq!(next_charge_check(now, 23, 59), Some(now + 4 * 60));
    assert_eq!(
        next_charge_check(now + 4 * 60, 23, 59),
        Some(now + 4 * 60 + DAY)
    );
}
