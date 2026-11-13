use saic_rust_notify::{
    config::Config,
    mqtt::{run, SharedTelemetry},
    telemetry::{Source, Telemetry},
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, RwLock},
    time::{sleep, timeout},
};

async fn packet(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let kind = stream.read_u8().await.unwrap();
    let mut length = 0;
    let mut shift = 0;
    loop {
        let byte = stream.read_u8().await.unwrap();
        length |= ((byte & 127) as usize) << shift;
        if byte & 128 == 0 {
            break;
        }
        shift += 7;
        assert!(shift < 28);
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    (kind, body)
}
async fn connect(listener: &TcpListener) -> TcpStream {
    let (mut stream, _) = listener.accept().await.unwrap();
    let (kind, body) = packet(&mut stream).await;
    assert_eq!(kind, 0x10);
    assert_eq!(&body[..7], b"\0\x04MQTT\x04");
    assert_ne!(body[7] & 2, 0, "clean session must resubscribe");
    stream
}
async fn subscribe(stream: &mut TcpStream, accepted: bool) {
    stream.write_all(&[0x20, 2, 0, 0]).await.unwrap();
    let (kind, body) = packet(stream).await;
    assert_eq!(kind, 0x82);
    assert_eq!(&body[2..], b"\0\x06saic/#\0");
    stream
        .write_all(&[0x90, 3, body[0], body[1], if accepted { 0 } else { 0x80 }])
        .await
        .unwrap();
}
async fn publish(stream: &mut TcpStream, topic: &str, value: &[u8], retained: bool) {
    let mut body = Vec::new();
    body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    body.extend_from_slice(value);
    assert!(body.len() < 128);
    stream
        .write_all(&[0x30 | u8::from(retained), body.len() as u8])
        .await
        .unwrap();
    stream.write_all(&body).await.unwrap();
}
async fn wait_connected(state: &SharedTelemetry, expected: bool) {
    loop {
        if state.read().await.is_connected() == expected {
            return;
        }
        sleep(Duration::from_millis(5)).await;
    }
}
async fn wait_value(state: &SharedTelemetry, expected: &str) {
    loop {
        if state
            .read()
            .await
            .latest(
                &Source::Vehicle("saic/fleet/vehicles/CAR".into()),
                "vehicle/drivetrain/soc",
            )
            .is_some_and(|r| r.value == expected)
        {
            return;
        }
        sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn broker_rejection_subscription_rejection_reconnect_and_shutdown() {
    timeout(Duration::from_secs(20), async {
        // Synthetic loopback broker; no environment, external broker, or SAIC access.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        let config = Config::load(|key| {
            Ok(match key {
                "MQTT_HOST" => Some("127.0.0.1".into()),
                "MQTT_PORT" => Some(port.clone()),
                "MQTT_USERNAME" => Some("test".into()),
                "MQTT_PASSWORD" => Some("test".into()),
                "MQTT_ACCOUNT" => Some("fleet".into()),
                _ => None,
            })
        })
        .unwrap();
        let state = Arc::new(RwLock::new(Telemetry::default()));
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(run(config, state.clone(), async {
            let _ = stopped.await;
        }));
        let mut rejected = connect(&listener).await;
        rejected.write_all(&[0x20, 2, 0, 5]).await.unwrap();
        drop(rejected);
        let mut rejected = connect(&listener).await;
        assert!(!state.read().await.is_connected());
        subscribe(&mut rejected, false).await;
        drop(rejected);
        let mut broker = connect(&listener).await;
        assert!(!state.read().await.is_connected());
        subscribe(&mut broker, true).await;
        wait_connected(&state, true).await;
        publish(
            &mut broker,
            "saic/fleet/vehicles/CAR/drivetrain/soc",
            b"70",
            true,
        )
        .await;
        wait_value(&state, "70").await;
        assert_eq!(
            state.read().await.control_base(),
            Ok("saic/fleet/vehicles/CAR")
        );
        drop(broker);
        wait_connected(&state, false).await;
        assert!(state.read().await.control_base().is_err());
        let mut broker = connect(&listener).await;
        subscribe(&mut broker, true).await;
        wait_connected(&state, true).await;
        // Malformed and filtered traffic cannot corrupt state or discover vehicles.
        publish(
            &mut broker,
            "saic/fleet/vehicles/CAR/drivetrain/soc",
            &[255],
            false,
        )
        .await;
        publish(
            &mut broker,
            "saic/fleet/vehicles/CAR/drivetrain/soc",
            b"{broken",
            false,
        )
        .await;
        publish(
            &mut broker,
            "saic/other/vehicles/OTHER/drivetrain/soc",
            b"0",
            false,
        )
        .await;
        publish(
            &mut broker,
            "saic/fleet/vehicles/CAR/drivetrain/soc",
            b"71",
            false,
        )
        .await;
        wait_value(&state, "71").await;
        assert_eq!(state.read().await.vehicle_bases().count(), 1);
        stop.send(()).unwrap();
        task.await.unwrap();
        assert!(!state.read().await.is_connected());
        let mut byte = [0];
        assert_eq!(broker.read(&mut byte).await.unwrap(), 0);
    })
    .await
    .expect("MQTT lifecycle test timed out");
}
