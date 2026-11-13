use saic_rust_notify::{config::Config, telemetry::Telemetry, vehicle};
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
    let client = match vehicle::client(&config) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("Configuration error: {error}");
            std::process::exit(2);
        }
    };
    // Only our own fixed messages are logged, even at TRACE. HTTP dependency
    // diagnostics can expose credentials and payloads.
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(Targets::new().with_target("saic_rust_notify", config.log_level)),
        )
        .init();
    let state = Arc::new(RwLock::new(Telemetry::default()));
    let (controls, commands) =
        saic_rust_notify::controls::Controls::new(state.clone(), config.stale_after);
    let app = saic_rust_notify::http::router(controls.clone(), config.api_token.as_deref());
    let listener =
        match tokio::net::TcpListener::bind((config.http_host.as_str(), config.http_port)).await {
            Ok(listener) => listener,
            Err(_) => {
                eprintln!("Could not bind HTTP listener");
                std::process::exit(2);
            }
        };
    let notifications = saic_rust_notify::notify::run(
        config.notify.clone(),
        state.clone(),
        config.stale_after,
        controls.preheating.subscribe(),
    );
    tokio::select! {
        _ = notifications => {},
        _ = vehicle::run(client, config, state.clone(), commands, shutdown()) => {},
        result = axum::serve(listener, app) => {
            if result.is_err() { tracing::error!("HTTP server stopped unexpectedly"); }
        }
    }
    state.write().await.set_connected(false);
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
