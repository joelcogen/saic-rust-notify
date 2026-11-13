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

The executable subscribes to `saic/#` with QoS 0 over plain MQTT/TCP and
serves authenticated HTTP status and vehicle controls. Commands use the gateway's
MQTT protocol; no SAIC API functionality is duplicated from `saic-client`.
Notify delivery is separate work; no web dashboard is served.

Run with Rust/Cargo and process environment variables (the service does not read
`.env` files):

```sh
cargo build --locked --release
# Supply the settings below through your service manager or shell environment.
./target/release/saic-rust-notify
```

| Variable | Default / requirement |
| --- | --- |
| `HTTP_HOST` | `127.0.0.1`; bind interface for HTTP |
| `HTTP_PORT` | `8080`, integer 1–65535 |
| `CLIMATE_API_TOKEN` | Bearer token; unset/empty rejects every HTTP request |
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
discovered vehicle, including across accounts. Commands are refused when discovery is incomplete or ambiguous.

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

## HTTP API

All requests require `Authorization: Bearer <CLIMATE_API_TOKEN>`. Token digests
are compared in constant time. Responses carry `Cache-Control: no-store`,
`X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, frame denial and
a restrictive Content Security Policy. Use a trusted TLS reverse proxy for
access beyond loopback; this listener speaks HTTP.

- `GET /api/status`: connection state and readings with `value`, `retained`, and
  `fresh`. `sources` preserves account/vehicle isolation; ambiguous `readings` keys
  are prefixed with their source to avoid collisions. Unique keys retain the
  Python format, such as `vehicle/climate/interiorTemperature`.
- `POST /climate/start`: optional JSON object with only `temperature`. Numbers
  are Celsius; text supports decimal comma, signs and optional °C/°F (also º,
  plain C/F and Shortcuts whitespace). Booleans, null and nonfinite values fail.
  Without an override, use the lower valid fresh interior/exterior temperature;
  fail with 503 if neither is fresh.
- `POST /refresh`: publish `refresh/mode/set=force`, return 202 after PUBACK.
- `POST /locate`: publish `location/findMyCar/set=lights_only`, never horn;
  return 200 only after PUBACK and a new non-retained `Success` result.

Bodies are limited to 4096 bytes and a five-second read timeout. Transfer-Encoding,
malformed JSON, unknown keys and bodies on status/refresh/locate return 400.
Authentication failures return 401; unknown routes (including dashboard assets)
return 404. Vehicle unavailability, rejection, partial failure and unknown
outcomes return 503 with distinct messages. Raw gateway errors are not exposed.

Climate starts front defrost at <=4°C, otherwise normal remote climate. At <=6°C
it then heats left seat, right seat and steering wheel; at <=4°C it finally starts
rear defrost. These ordered commands are sent in one non-retained QoS 1 batch
with a unique UUID requestId. Success requires PUBACK within five seconds and a
matching non-retained result with the complete ordered success list within 360
seconds. Locate waits up to 45 seconds for its result; its gateway protocol has
no request ID, so avoid concurrent external locate callers. Climate and locate
are mutually exclusive within this service. Refresh also requires PUBACK within
five seconds. Disconnects and missing acknowledgments/results are unknown
outcomes: earlier commands may have succeeded; check the vehicle before retrying.
Commands are never automatically replayed across reconnection.

An in-process `Controls::preheating` watch signal starts only after acknowledged
full climate success. It is the integration point for the subsequent Notify Live
Activity implementation; this story does not send Notify notifications.

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
