# SAIC Rust Notify

SAIC Rust Notify connects vehicle data and controls to MQTT and Notify! widgets
and Live Activities. Its goal is to provide a small, self-hosted service for
viewing useful vehicle information and requesting selected vehicle actions,
with configuration supplied through environment variables.

The service is intended to work with the SAIC MQTT Gateway and an MQTT broker.
Notify! integrations are optional. The web dashboard is outside the project's
scope for now.

The SAIC Rust client lives in [`../saic-rust-client`](../saic-rust-client) and
provides SAIC API and vehicle operations. See its documentation for supported
operations and development guidance.
