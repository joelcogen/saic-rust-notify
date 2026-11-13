use chrono::{TimeZone, Utc};
use saic_rust_notify::{config::Config, telemetry::*};
use std::time::{Duration, Instant};

fn config(values: &[(&str, &str)]) -> Result<Config, String> {
    Config::load(|key| {
        Ok(values
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string()))
    })
}
#[test]
fn environment_defaults_and_validation_do_not_expose_values() {
    let base = [
        ("MQTT_USERNAME", "test-user"),
        ("MQTT_PASSWORD", "synthetic-secret"),
    ];
    let c = config(&base).unwrap();
    assert_eq!(c.host, "localhost");
    assert_eq!(c.port, 1883);
    assert_eq!(c.stale_after, Duration::from_secs(600));
    assert_eq!(c.stale_check, Duration::from_secs(30));
    assert!(config(&[]).unwrap_err_safe().contains("MQTT_USERNAME"));
    for (key, value) in [
        ("MQTT_PORT", "65536"),
        ("MQTT_PORT", "0"),
        ("MQTT_PORT", "secret-invalid"),
        ("STALE_AFTER_SECONDS", "-1"),
        ("STALE_CHECK_SECONDS", "0"),
        ("MQTT_ACCOUNT", "a/+"),
        ("MQTT_HOST", " "),
        ("LOG_LEVEL", "secret-invalid"),
    ] {
        let mut values = base.to_vec();
        values.push((key, value));
        let error = config(&values).unwrap_err_safe();
        assert!(error.contains(key));
        assert!(!error.contains("secret-invalid"));
    }
    assert_eq!(
        config(&[
            ("MQTT_USERNAME", "u"),
            ("MQTT_PASSWORD", "p"),
            ("MQTT_ACCOUNT", "fleet")
        ])
        .unwrap()
        .account
        .as_deref(),
        Some("fleet")
    );
}
// Config intentionally cannot implement Debug, including for test assertion helpers.
trait ConfigError {
    fn unwrap_err_safe(self) -> String;
}
impl ConfigError for Result<Config, String> {
    fn unwrap_err_safe(self) -> String {
        match self {
            Err(e) => e,
            Ok(_) => panic!("expected invalid config"),
        }
    }
}

#[test]
fn parsing_and_account_routing() {
    let message = decode(
        "saic/fleet/vehicles/CAR/drivetrain/soc",
        b"70",
        Some("fleet"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        message.source,
        Source::Vehicle("saic/fleet/vehicles/CAR".into())
    );
    assert_eq!(message.key, "vehicle/drivetrain/soc");
    assert_eq!(message.value, "70");
    assert!(decode("saic/other/status", b"ok", Some("fleet"))
        .unwrap()
        .is_none());
    assert_eq!(
        decode("saic/fleet/status", b"online", None)
            .unwrap()
            .unwrap()
            .key,
        "account/status"
    );
    assert_eq!(
        decode("saic/fleet/vehicles/CAR", b"{}", None)
            .unwrap()
            .unwrap()
            .key,
        "vehicle/root"
    );
    for topic in [
        "other/a/status",
        "saic/a",
        "saic/a/vehicles",
        "saic//status",
        "saic/a/vehicles//x",
        "saic/a/+/x",
        "saic/a/status/",
    ] {
        assert!(matches!(decode(topic, b"ok", None), Err(Reject::Topic)));
    }
    assert!(matches!(
        decode("saic/a/status", &[255], None),
        Err(Reject::Utf8)
    ));
    for body in ["{broken", " [broken", "{\"n\":NaN}"] {
        assert!(matches!(
            decode("saic/a/status", body.as_bytes(), None),
            Err(Reject::Json)
        ));
    }
    assert!(decode("saic/a/status", b" [1,true]", None)
        .unwrap()
        .unwrap()
        .value
        .is_array());
    assert!(matches!(
        decode("saic/a/status", &vec![0; MAX_PAYLOAD + 1], None),
        Err(Reject::TooLarge)
    ));
}
fn put(
    state: &mut Telemetry,
    base: &str,
    path: &str,
    value: &str,
    retained: bool,
    now: Instant,
) -> bool {
    state
        .ingest(
            decode(&format!("{base}/{path}"), value.as_bytes(), None)
                .unwrap()
                .unwrap(),
            retained,
            now,
        )
        .unwrap()
}
const BASE: &str = "saic/fleet/vehicles/CAR";
#[test]
fn freshness_retained_timestamps_and_disconnect() {
    let mut state = Telemetry::default();
    state.set_connected(true);
    let now = Instant::now();
    let wall = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    let ttl = Duration::from_secs(600);
    let source = Source::Vehicle(BASE.into());
    put(&mut state, BASE, "drivetrain/soc", "70", true, now);
    assert!(state
        .fresh(&source, "vehicle/drivetrain/soc", now, wall, ttl)
        .is_none());
    put(
        &mut state,
        BASE,
        "refresh/lastVehicleState",
        "2026-01-01T12:00:00Z",
        true,
        now,
    );
    assert!(state
        .fresh(&source, "vehicle/drivetrain/soc", now, wall, ttl)
        .is_none());
    for stamp in ["2026-01-01T11:50:00Z", "2026-01-01T13:00:00+01:00"] {
        put(
            &mut state,
            BASE,
            "refresh/lastChargeState",
            stamp,
            true,
            now,
        );
        assert!(state
            .fresh(&source, "vehicle/drivetrain/soc", now, wall, ttl)
            .is_some());
    }
    for stamp in [
        "2026-01-01T11:49:59Z",
        "2026-01-01T12:00:01Z",
        "invalid",
        "2026-01-01T12:00:00",
    ] {
        put(
            &mut state,
            BASE,
            "refresh/lastChargeState",
            stamp,
            true,
            now,
        );
        assert!(state
            .fresh(&source, "vehicle/drivetrain/soc", now, wall, ttl)
            .is_none());
    }
    put(&mut state, BASE, "drivetrain/soc", "71", false, now);
    // A parseable naive timestamp explicitly prevents the fallback, matching Python.
    assert!(state
        .fresh(&source, "vehicle/drivetrain/soc", now, wall, ttl)
        .is_none());
    put(
        &mut state,
        BASE,
        "refresh/lastChargeState",
        "invalid",
        true,
        now,
    );
    assert!(state
        .fresh(&source, "vehicle/drivetrain/soc", now + ttl, wall, ttl)
        .is_some());
    assert!(state
        .fresh(
            &source,
            "vehicle/drivetrain/soc",
            now + ttl + Duration::from_nanos(1),
            wall,
            ttl
        )
        .is_none());
    state.set_connected(false);
    assert!(state
        .fresh(&source, "vehicle/drivetrain/soc", now, wall, ttl)
        .is_none());
    assert_eq!(
        state
            .latest(&source, "vehicle/drivetrain/soc")
            .unwrap()
            .value,
        "71"
    );
    assert!(
        !state
            .latest(&source, "vehicle/drivetrain/soc")
            .unwrap()
            .retained
    );
}
#[test]
fn sources_are_isolated_and_controls_require_exactly_one_vehicle() {
    let mut state = Telemetry::default();
    let now = Instant::now();
    assert_eq!(state.control_base(), Err(ControlError::Disconnected));
    state.set_connected(true);
    assert_eq!(
        state.control_base(),
        Err(ControlError::SingleVehicleRequired)
    );
    put(&mut state, BASE, "drivetrain/soc", "70", true, now);
    assert_eq!(state.control_base(), Ok(BASE));
    let other = "saic/other/vehicles/CAR";
    put(&mut state, other, "drivetrain/soc", "20", false, now);
    put(
        &mut state,
        other,
        "refresh/lastChargeState",
        "2026-01-01T12:00:00Z",
        true,
        now,
    );
    assert_eq!(
        state.control_base(),
        Err(ControlError::SingleVehicleRequired)
    );
    let source = Source::Vehicle(BASE.into());
    assert_eq!(
        state
            .latest(&source, "vehicle/drivetrain/soc")
            .unwrap()
            .value,
        "70"
    );
    assert!(state
        .fresh(
            &source,
            "vehicle/drivetrain/soc",
            now,
            Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap(),
            Duration::from_secs(600)
        )
        .is_none());
    state.set_connected(false);
    state.set_connected(true);
    assert_eq!(state.vehicle_bases().count(), 2);
    assert_eq!(
        state.control_base(),
        Err(ControlError::SingleVehicleRequired)
    );
}
#[test]
fn stale_warning_is_once_per_outage_and_only_accepted_traffic_resumes_it() {
    let mut state = Telemetry::default();
    let now = Instant::now();
    let ttl = Duration::from_secs(10);
    assert!(state.check_stale(now, ttl));
    assert!(!state.check_stale(now, ttl));
    assert!(put(&mut state, BASE, "status", "ok", true, now));
    assert!(!state.check_stale(now + Duration::from_secs(9), ttl));
    assert!(state.check_stale(now + ttl, ttl));
    assert!(!state.check_stale(now + ttl, ttl));
    assert!(decode("invalid", b"ok", None).is_err());
    assert!(!state.check_stale(now + ttl, ttl));
    assert!(put(&mut state, BASE, "status", "ok", false, now + ttl));
    assert!(!put(&mut state, BASE, "status", "ok", false, now + ttl));
}

#[test]
fn capacity_rejection_preserves_state_and_disables_controls() {
    let mut state = Telemetry::default();
    state.set_connected(true);
    let now = Instant::now();
    put(&mut state, BASE, "status", "ok", false, now);
    for i in 0..127 {
        let message = decode(&format!("saic/account{i}/status"), b"online", None)
            .unwrap()
            .unwrap();
        state.ingest(message, false, now).unwrap();
    }
    assert_eq!(state.control_base(), Ok(BASE));
    let second = decode("saic/fleet/vehicles/SECOND/status", b"online", None)
        .unwrap()
        .unwrap();
    assert_eq!(state.ingest(second, false, now), Err(Reject::Capacity));
    assert_eq!(state.control_base(), Err(ControlError::DiscoveryIncomplete));
    put(&mut state, BASE, "status", "updated", false, now);
    assert_eq!(
        state
            .latest(&Source::Vehicle(BASE.into()), "vehicle/status")
            .unwrap()
            .value,
        "updated"
    );
    state.set_connected(false);
    state.set_connected(true);
    assert_eq!(state.control_base(), Err(ControlError::DiscoveryIncomplete));
}
