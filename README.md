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

## MQTT consumer

The current executable implements the MQTT telemetry portion of the rewrite.
It subscribes to `saic/#` with QoS 0, using plain MQTT over TCP, and makes no
SAIC API requests or vehicle commands. HTTP endpoints, commands and Notify!
integration are separate work; no web dashboard is served. MQTT transport and
freshness belong here, not in `saic-client`; no SAIC API/domain functionality is
duplicated or needed by this consumer.

Run with Rust/Cargo and process environment variables (the service does not read
`.env` files):

```sh
cargo build --locked --release
# Supply the settings below through your service manager or shell environment.
./target/release/saic-rust-notify
```

| Variable | Default / requirement |
| --- | --- |
| `MQTT_HOST` | `localhost` |
| `MQTT_PORT` | `1883`, integer 1–65535 |
| `MQTT_USERNAME` | Required, nonempty broker username |
| `MQTT_PASSWORD` | Required, nonempty broker password |
| `MQTT_CLIENT_ID` | `saic-telemetry-consumer`; must differ from the gateway's ID |
| `MQTT_ACCOUNT` | Optional account topic segment; unset/empty accepts all accounts |
| `STALE_AFTER_SECONDS` | `600`, positive integer up to 4294967295 |
| `STALE_CHECK_SECONDS` | `30`, positive integer up to 4294967295 |
| `LOG_LEVEL` | `INFO`; ERROR, WARN/WARNING, INFO, DEBUG, TRACE supported |

Use the same broker as the SAIC MQTT Gateway with its default `saic` prefix.
The gateway's `MQTT_USER` corresponds to this service's `MQTT_USERNAME`.
`MQTT_ACCOUNT` filters topics, not authentication. Keep secrets in ignored
`runtime.env` or your service manager. Configuration errors name the setting
without printing its value. Logs exclude accounts, vehicle identifiers, payloads,
broker credentials and dependency diagnostics, including at TRACE.

Connection failures retry with delays from 1 to 60 seconds. Every connection
subscribes again; state becomes connected only after subscription acceptance.
SIGINT/SIGTERM stop the consumer. Cached state survives reconnects but not process
restarts. Controls can obtain a base only while connected with exactly one
discovered vehicle, including across accounts. This is a validation API only;
the executable never publishes vehicle commands.

### State and freshness

`saic/<account>/vehicles/<id>/<path>` maps to `vehicle/<path>` (or `vehicle/root`
for the vehicle base); other account topics map to `account/<path>`. State is
isolated per account/vehicle to prevent one vehicle's timestamps or values from
overwriting another's. Each reading stores its value, retained flag and monotonic
receipt time under a shared read/write lock. Scalar payloads remain strings;
objects and arrays decode as JSON. Invalid UTF-8, JSON and topic shapes are
rejected without changing state. Account filtering happens before payload parsing.

Freshness follows the Python consumer:

- SoC, target SoC, charging, power, remaining charging time and charger connection
  use `refresh/lastChargeState`; other vehicle readings use `refresh/lastVehicleState`.
- A timezone-qualified gateway timestamp between now and the freshness threshold
  permits retained values. Future or old timestamps do not certify freshness.
- Otherwise a non-retained reading can be fresh by monotonic receipt age. A
  parseable timestamp without a timezone explicitly prevents this fallback,
  matching the reference. Missing/invalid timestamps allow the receipt fallback.
- Disconnected readings are never returned as fresh. Cached values remain available
  separately. Freshness thresholds are inclusive; stale-traffic warnings start
  at the threshold. Retained or account traffic counts as received traffic but
  does not by itself certify vehicle readings as fresh.

While connected, a periodic check logs one stale warning until accepted traffic
resumes, then logs a resume message. This traffic check is separate from per-field
freshness. On startup, the first check warns if no matching messages have arrived.

Resource limits are 256 KiB per payload, 4096 bytes per topic, 128 sources, 4096
stored readings and 16 MiB of serialized values/keys (in-memory JSON overhead is
additional). Excess messages are rejected; existing entries remain. A state
capacity rejection disables control-base validation until restart, so incomplete
discovery cannot accidentally authorize a single-vehicle control.

## Development

All tests use synthetic data. The MQTT integration test runs a minimal broker on
an ephemeral loopback port and exercises rejection, subscription, reconnect and
shutdown. It does not read runtime credentials or contact SAIC or a vehicle.

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```
