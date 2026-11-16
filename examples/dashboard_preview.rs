//! Offline dashboard preview. No credentials, client session, or vehicle access.
use saic_rust_notify::{
    controls::Controls,
    http::router,
    telemetry::{Message, Source, Telemetry},
    vehicle::Action,
};
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

#[tokio::main]
async fn main() {
    let state = Arc::new(RwLock::new(Telemetry::default()));
    {
        let mut data = state.write().await;
        data.set_connected(true);
        for (key, value) in [
            (
                "refresh/lastVehicleState",
                json!((chrono::Utc::now() - chrono::Duration::minutes(2)).to_rfc3339()),
            ),
            ("doors/locked", json!(true)),
            ("drivetrain/parked", json!(true)),
            ("drivetrain/charging", json!(false)),
            ("drivetrain/soc", json!(78)),
            ("climate/interiorTemperature", json!(18)),
            ("climate/remoteClimateState", json!("off")),
            ("climate/frontWindowDefrosterHeating", json!(false)),
            ("climate/rearWindowDefrosterHeating", json!(false)),
            ("climate/steeringWheelHeating", json!(false)),
            ("climate/heatedSeatsFrontLeftLevel", json!(3)),
            ("climate/heatedSeatsFrontRightLevel", json!(3)),
            ("climate/heatedSeatsRearLeftLevel", json!(0)),
            ("climate/heatedSeatsRearRightLevel", json!(0)),
            ("doors/driver", json!(false)),
            ("doors/passenger", json!(false)),
            ("doors/rearLeft", json!(false)),
            ("doors/rearRight", json!(false)),
            ("doors/bonnet", json!(false)),
            ("doors/boot", json!(false)),
            ("windows/driver", json!(false)),
            ("windows/passenger", json!(false)),
            ("windows/rearLeft", json!(false)),
            ("windows/rearRight", json!(false)),
            ("tyres/frontLeftPressure", json!(2.6)),
            ("tyres/frontRightPressure", json!(2.6)),
            ("tyres/rearLeftPressure", json!(2.6)),
            ("tyres/rearRightPressure", json!(2.6)),
        ] {
            data.ingest(
                Message {
                    source: Source::Vehicle("SYNTHETIC".into()),
                    key: format!("vehicle/{key}"),
                    value,
                },
                false,
                Instant::now(),
            )
            .unwrap();
        }
    }
    let (controls, mut receiver) = Controls::new(state.clone(), Duration::from_secs(600));
    tokio::spawn(async move {
        while let Some(request) = receiver.recv().await {
            tokio::time::sleep(Duration::from_millis(700)).await;
            let (key, value, response) = match request.action {
                Action::Refresh => (
                    "refresh/lastVehicleState",
                    json!(chrono::Utc::now().to_rfc3339()),
                    "Demo vehicle data refreshed",
                ),
                Action::Climate(_) => (
                    "climate/remoteClimateState",
                    json!("on"),
                    "Demo climate started",
                ),
                Action::Locate => {
                    let _ = request.reply.send(Ok("Demo lights flashed"));
                    continue;
                }
            };
            state
                .write()
                .await
                .ingest(
                    Message {
                        source: Source::Vehicle("SYNTHETIC".into()),
                        key: format!("vehicle/{key}"),
                        value,
                    },
                    false,
                    Instant::now(),
                )
                .unwrap();
            let _ = request.reply.send(Ok(response));
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8099")
        .await
        .unwrap();
    println!("Synthetic dashboard: http://127.0.0.1:8099 · token: demo-token");
    axum::serve(listener, router(controls, Some("demo-token")))
        .await
        .unwrap();
}
