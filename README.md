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
Optional Notify! workers push a dashboard widget and charging/preheating Live
Activities. No web dashboard or HTML/CSS/JavaScript assets are served.

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
| `NOTIFY_DASHBOARD_URL` | Optional Notify dashboard widget push URL; unset/empty disables it |
| `NOTIFY_ACTIVITY_URL` | Optional Notify device Live Activity URL; unset/empty disables both activities |
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
full climate success. Notify delivery runs independently: failed or uncertain
notification creation does not change command success or repeat vehicle commands.

## Optional Notify! integration

Set the full dashboard push URL and/or device Live Activity URL provided by
Notify!, including their authentication query parameters. These are secrets;
keep them out of source control, access logs and screenshots. Use HTTPS outside
a trusted local network. URLs must be HTTP(S), without userinfo or fragments.
Requests have a 15-second timeout, bounded response bodies and no redirects;
logs never include endpoint URLs, response bodies or raw HTTP client errors.
TLS uses Rustls and bundled public trust roots (no Python or OpenSSL runtime).

- The dashboard widget checks every five seconds, sends changed payloads and
  refreshes unchanged payloads every five minutes. It includes SOC, doors,
  charging/plugged/driving/parked status, inside/outside temperature and A/C.
  Cached values remain visible with `Last known / stale` when required readings
  are stale or disconnected. The widget is unrelated to the deferred web dashboard.
- `Charging Big Red` discovers an existing activity by title, then checks every
  30 seconds. Only fresh charging data drives updates and completion. The payload
  includes SOC progress, absolute power in kW and remaining seconds (capped at
  24 hours). Stale fields display `?`; unknown charging state causes no update
  or completion. Reaching fresh target SOC ends with `done`; charging off ends
  with `stopped`. After target completion or HTTP 410, creation stays suppressed
  until a fresh charging-off reading resets it.
- `Preheating Big Red` starts after the complete successful climate batch and
  lasts ten minutes, updating at most once per minute. It shows cached interior
  temperature, seat warming and defrost indicators, with temperature progress
  from 0 to 20. As in the Python bridge, cached climate indicators may be stale;
  absent temperature displays `?`. Completion deletes with `status=done` and
  `keepFor=0`. On restart it recovers the remote `endsAt` deadline, capped to ten
  minutes; HTTP 410 clears the activity until a new successful climate request.
- Both activity workers discover by title (fetching individual details when
  necessary), reject ambiguous matches, and explicitly create with `new=1`.
  An uncertain creation is followed by discovery before another create attempt.
  Normal errors retry after 30 seconds. HTTP 429 honors numeric `Retry-After`,
  bounded to 30–3600 seconds (default 60); dashboard delivery uses the same
  backoff. Workers are independent so a slow endpoint does not hold telemetry
  locks, delay vehicle commands or block the other notification workers.

Use one service instance per Notify device/vehicle and keep activity titles
unique. With zero or multiple discovered vehicles, notification readings are
unavailable rather than mixing vehicles. Local notification state is in memory;
activity discovery recovers remote activities after restart. A new successful
climate request resets its ten-minute deadline. The service never starts vehicle
commands to recover a notification.

## Container deployment

Build from this repository (no parent-directory build context is required):

```sh
docker build -t saic-rust-notify:local .
docker run --rm --name saic-rust-notify \
  --env-file runtime.env \
  --publish 127.0.0.1:8080:8080 \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  saic-rust-notify:local
```

Create `runtime.env` locally with your own values for `MQTT_HOST`,
`MQTT_USERNAME`, `MQTT_PASSWORD`, `MQTT_ACCOUNT` and `CLIMATE_API_TOKEN`, plus
optional Notify URLs. Docker explicitly loads this file; the executable never
loads it or any `.env` file implicitly. The build context allowlist excludes
credentials, tests, dashboard assets and unrelated files. The final Debian image
contains only the compiled Rust service and base runtime libraries, runs as
UID/GID 10001, and contains no Python runtime or dependencies. HTTP binds to
`0.0.0.0:8080` inside the container; native execution defaults to loopback.
SIGTERM stops the service and cancels notification requests.

On a Docker network, use the broker service name as `MQTT_HOST`; `localhost`
inside a container refers to that container. Configure the SAIC MQTT Gateway
separately with the same broker, credentials and `saic` topic prefix. Its
`MQTT_USER` maps to this service's `MQTT_USERNAME`; each process needs a distinct
client ID. Configure `MQTT_ACCOUNT` to the gateway's account topic segment.
Climate requires a gateway version supporting `commands/batch/set` and matching
ordered results; an older gateway can time out after an unknown outcome. This
service does not log in to SAIC itself and needs no SAIC account password.

MQTT is plain TCP: use a private network and broker ACLs restricting this service
to the intended account's telemetry, result and command topics. Keep port 8080
private or behind a TLS reverse proxy. Bearer authorization is required even for
status; an absent token rejects all requests. Only explicitly requested HTTP
controls publish commands. Do not use real vehicle actions as deployment checks.

### SAIC client dependency choice

The local [`saic-client` 0.1.0](../saic-rust-client) crate is the reference for
SAIC API/domain operations. This service currently uses the gateway's MQTT
command protocol and performs no direct SAIC API operations, so it does not add
an unused Cargo dependency or duplicate login, transport or domain behavior.
A future direct-API integration should use
`saic-client = { version = "0.1.0", path = "../saic-rust-client" }` after reading
that crate's `AGENTS.md` and operation docs; it would also require an explicit
sibling-crate container build context. Notify HTTP delivery uses `reqwest`
0.12.28 with Rustls; exact transitive versions are committed in `Cargo.lock`.

## Development

All tests use synthetic data. The MQTT integration test runs a minimal broker on
an ephemeral loopback port and exercises rejection, subscription, reconnect and
shutdown. It does not read runtime credentials or contact SAIC or a vehicle. Notify tests
use a loopback HTTP server and explicit test clocks to cover payloads, stale
telemetry, deduplication, uncertain creation, discovery, completion, 410/429/error
backoff and isolation from command success. No tests call real Notify endpoints.

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Container smoke verification uses only synthetic broker settings and sends no
vehicle commands (requires Docker and curl):

```sh
docker build -t saic-rust-notify:local .
./tests/container.sh saic-rust-notify:local
```
