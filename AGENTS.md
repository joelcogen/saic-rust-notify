# Agent guidance

This project is a Rust rewrite of the Python service in
[`../saic-mqtt-service`](../saic-mqtt-service). Use that directory as the
behavioral reference for MQTT telemetry handling, freshness, HTTP endpoints,
vehicle commands, Notify! widgets and Live Activities, configuration, and
deployment. Preserve its features except the web dashboard, which is deferred.

Use the local SAIC Rust client in [`../saic-rust-client`](../saic-rust-client)
for SAIC API/domain behavior where applicable; do not duplicate its
functionality. Read that crate's `AGENTS.md` and relevant docs before changing
how it is used.

Keep credentials and real account data out of source, commits, and tool output.
Use synthetic fixtures for tests. Do not contact SAIC production or a vehicle
unless the user explicitly requests that specific check; available credentials
do not grant permission. Do not start vehicle actions as part of development.

Keep the service configuration and documentation aligned with behavior. Do not
add or serve the deferred web dashboard unless the user asks for it.
