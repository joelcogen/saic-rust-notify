use crate::{
    config::Config,
    telemetry::{decode, Telemetry, MAX_PAYLOAD},
};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS, SubscribeReasonCode};
use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

pub type SharedTelemetry = Arc<RwLock<Telemetry>>;

/// Polling, reconnects, and all state updates have one owner. Readers use a shared lock.
/// Dropping the run future closes the broker connection; normal shutdown also marks state offline.
pub async fn run(config: Config, state: SharedTelemetry, shutdown: impl Future<Output = ()>) {
    tokio::pin!(shutdown);
    tokio::select! {
        _ = &mut shutdown => {},
        _ = consume(&config, &state) => {},
    }
    state.write().await.set_connected(false);
    tracing::info!("MQTT consumer stopped");
}

async fn consume(config: &Config, state: &SharedTelemetry) {
    let mut delay = Duration::from_secs(1);
    let mut stale_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + config.stale_check,
        config.stale_check,
    );
    stale_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let mut options = MqttOptions::new(&config.client_id, &config.host, config.port);
        options.set_credentials(&config.username, &config.password);
        options.set_keep_alive(Duration::from_secs(60));
        options.set_clean_session(true);
        options.set_max_packet_size(MAX_PAYLOAD + 8192, MAX_PAYLOAD + 8192);
        let (client, mut events) = AsyncClient::new(options, 10);
        loop {
            tokio::select! {
                _ = stale_tick.tick() => {
                    let mut state = state.write().await;
                    if state.is_connected() && state.check_stale(Instant::now(), config.stale_after) {
                        tracing::warn!("No SAIC MQTT messages received within the freshness interval");
                    }
                }
                event = events.poll() => match event {
                    Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                        if client.subscribe("saic/#", QoS::AtMostOnce).await.is_err() { break; }
                    }
                    Ok(Event::Incoming(Incoming::SubAck(ack))) => {
                        if ack.return_codes.len() != 1 || matches!(ack.return_codes[0], SubscribeReasonCode::Failure) {
                            tracing::warn!("MQTT subscription rejected; reconnecting");
                            break;
                        }
                        state.write().await.set_connected(true);
                        delay = Duration::from_secs(1);
                        tracing::info!("Connected to MQTT broker and subscribed to SAIC topics");
                    }
                    Ok(Event::Incoming(Incoming::Publish(message))) => {
                        match decode(&message.topic, &message.payload, config.account.as_deref()) {
                            Ok(Some(message_value)) => {
                                match state.write().await.ingest(message_value, message.retain, Instant::now()) {
                                    Ok(true) => tracing::info!("MQTT messages resumed"),
                                    Ok(false) => {},
                                    Err(_) => tracing::warn!("Discarding MQTT message: state capacity exceeded"),
                                }
                            }
                            Ok(None) => {},
                            Err(reason) => tracing::warn!(?reason, "Discarding malformed MQTT message"),
                        }
                    }
                    Ok(Event::Incoming(Incoming::Disconnect)) | Err(_) => break,
                    _ => {},
                }
            }
        }
        state.write().await.set_connected(false);
        // Never log library errors: they may contain broker addresses or packet data.
        tracing::warn!("MQTT connection unavailable; reconnecting");
        drop(client);
        drop(events);
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(60));
    }
}
