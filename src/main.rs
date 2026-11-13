use saic_rust_notify::{config::Config, mqtt, telemetry::Telemetry};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing_subscriber::{filter::Targets, prelude::*};

#[tokio::main]
async fn main() {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Configuration error: {error}");
            std::process::exit(2);
        }
    };
    // Only our own fixed messages are logged, even at TRACE. MQTT dependency
    // diagnostics can expose credentials and payloads.
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(Targets::new().with_target("saic_rust_notify", config.log_level)),
        )
        .init();
    mqtt::run(
        config,
        Arc::new(RwLock::new(Telemetry::default())),
        shutdown(),
    )
    .await;
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
