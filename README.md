# SAIC Rust Notify

A self-hosted Rust service that reads vehicle telemetry and sends vehicle controls
directly through the local [`saic-client`](../saic-rust-client) crate. It serves an
authenticated HTTP API and optional Notify! widgets and Live Activities. No other
local service is required. A browser dashboard is served at `/`.

## Run and configure

Check out `saic-rust-client` alongside this repository, then:

```sh
cargo build --locked --release
./target/release/saic-rust-notify
```

The executable reads `.env` from its current directory only. Process environment
variables take precedence, including empty values. Keep credentials in the ignored
`.env` or `runtime.env`, never in source control. Configuration errors identify
settings without printing values; logs exclude credentials, vehicle identifiers,
API responses and dependency diagnostics, including at TRACE. Session failures log
the stage (login, discovery, or vehicle_selection), an allowlisted error category,
and HTTP/SAIC numeric error codes when available. Raw error messages are never
logged. A vehicle_selection failure means discovery returned no usable matching
vehicle, an invalid list, or more than 128 entries.

| Variable | Default / requirement |
| --- | --- |
| `SAIC_USERNAME` | Required, nonempty SAIC email account |
| `SAIC_PASSWORD` | Required, nonempty SAIC password |
| `SAIC_VIN` | Optional vehicle selection; must belong to the authenticated account |
| `SAIC_BASE_URL` | Optional HTTPS API root override; defaults to the client's MG Europe profile |
| `STALE_AFTER_SECONDS` | `600`, positive integer up to 4294967295 |
| `HTTP_HOST` | `127.0.0.1`; bind interface for HTTP |
| `HTTP_PORT` | `8080`, integer 1–65535 |
| `CLIMATE_API_TOKEN` | Bearer token; unset/empty rejects all API and control requests |
| `NOTIFY_DASHBOARD_URL` | Optional Notify widget push URL; unset/empty disables it |
| `NOTIFY_ACTIVITY_URL` | Optional Notify device Live Activity URL; unset/empty disables both activities |
| `LOG_LEVEL` | `INFO`; ERROR, WARN/WARNING, INFO, DEBUG, TRACE supported |

Starting the service loads durable scheduling state, logs in, discovers account
vehicles, then polls live vehicle status and charging management data when due.
**Live polling can wake the vehicle.** Login or discovery failures retry after
60 seconds. Poll failures mark cached state disconnected and reauthenticate without resetting
vehicle deadlines. Each vehicle retries after 30, 60, 120, 240… seconds, capped at
its applicable parked interval. A successful refresh resets the backoff. HTTP and
notification workers remain available during retry waits.
SIGINT/SIGTERM stop the service and cancel pending network requests.

Without `SAIC_VIN`, all discovered vehicles are polled independently (maximum 128).
Controls and notifications require exactly one discovered vehicle. An explicit
VIN selects only that vehicle after verifying account membership. Invalid or
incomplete discovery fails closed. No controls run during discovery or polling.

Vehicle status timestamps must be present, valid, and within 15 minutes of the
current time (past or future). Rejected responses enter refresh failure backoff;
they preserve parking age and the last snapshot, which is marked stale, and
cannot trigger active polling. Charging data is not requested for rejected
vehicle responses.

### Fixed polling policy

Scheduling is independent per vehicle, following the gateway default precedence:
new observed activity, error recovery, charging, active HV battery, shutdown grace,
then parked. Running, remote climate or rear-window heating count as HV activity.
Active vehicles poll every 30 seconds. On transition to inactive, a parking epoch
starts: polls run every 120 seconds for a 600-second shutdown grace, then at
12, 24, 36 and 48 hours from that epoch, followed by 72 hours and every 24 hours.
Late polls skip missed deadlines rather than replaying them. A genuine observed
return to active or charging ends the parked period. Manual refreshes, reconnects
and ordinary refreshes never restart a known parking epoch.

Charging uses `36 × capacity_kWh / abs(power_kW)` seconds for 1% SOC, limited by
positive remaining charge time and bounded to 30 seconds–24 hours. Capacity uses
the client's reported battery capacity (tenths of kWh); model-specific capacity
corrections are unavailable. Low power (at most 1 kW), missing power or missing
capacity uses at least 120 seconds, retaining a longer prior charging interval.
An enabled vehicle charging schedule triggers a check ten minutes after its start
in the service's local timezone, matching the gateway default timezone behavior.
No extra schedule or activity-detection API reads are added.

All policy values are constants; there are no polling settings or modes. The
former interval environment setting is ignored. Status and Notify reads only use
stored telemetry, even when stale or missing. A successful climate batch triggers
one live refresh; `POST /refresh` also requests an earlier live update.

The service atomically saves per-vehicle parking epochs, deadlines and backoff in
`data/polling.json` relative to its working directory. Keys are hashed vehicle
identifiers; no credentials or telemetry are stored. Keep this directory writable
and persistent, run only one instance against it, and preserve it on upgrades.
Unreadable/corrupt state or write failures stop vehicle access rather than silently
resetting history. On first start without history, each vehicle is read once; an
inactive observation starts the 48-hour window at that observation because its
prior parking age is unknown. On restart with history, saved deadlines still
apply, and the SAIC cache supplies initial telemetry when available.

### Telemetry and freshness

`GET /api/status` preserves the `vehicle/...` reading keys used by Notify!, such
as `vehicle/climate/interiorTemperature`. Each source is a VIN; multiple vehicles
remain isolated. Ambiguous flattened keys are prefixed with their source.
Snapshots are in memory and are lost at restart. After login and discovery, the
service fetches each vehicle’s SAIC cache without requesting a live refresh.
Cached readings are displayed regardless of age with their original timestamp;
internal freshness checks still govern controls and notifications. Empty or failed
cache reads do not trigger a live fallback or change polling deadlines.

Vehicle readings use the API's UTC `statusTime`: old, future or absent timestamps
cannot certify freshness. Their `retained` flag is true to indicate that the API
may return an older observation. Charging management has no source timestamp;
its readings use monotonic receipt age and `retained=false`. Disconnected readings
are never fresh. Freshness thresholds are inclusive. Failed polls preserve the
last snapshot as stale; successful polls replace it so missing fields cannot be
revived by a new timestamp. Missing, invalid and unknown measurements are omitted,
not interpreted as zero or off.

The adapter maps interior temperature, cabin climate, front/rear seat heating, steering
and rear-window heating, locks, individual doors/windows, bonnet, tailgate, tire
pressures, parked/running state, SOC/target, charging/plug state, power and remaining charging
time to the existing Notify fields. Protocol scaling is converted to Celsius,
percent, kW and seconds. API transport, authentication, command payloads and event
continuation are owned by `saic-client`.

## Web dashboard

Open the service root (for example `http://127.0.0.1:8080/`) and enter your
`CLIMATE_API_TOKEN`. After validation, it is kept in this browser's localStorage
and reused across visits without prompting. It is sent only in the same-origin
Authorization header, never in a URL. Clear this site’s browser data to remove the saved token.
The dashboard is titled **Big Red** and uses a light red Tabler theme. The header
shows the last vehicle update and refresh control. Metrics sit above the car
diagram, and interior temperature appears beside the climate control.
A rejected token requires a replacement; clearing browser storage also clears it.
If the browser blocks storage, the page explains that persistence is unavailable.

The supplied car image has positioned status pills for tires, doors and windows,
bonnet/tailgate, front/rear heated seats, steering heat and defrost. The overview
shows lock, parked/driving/charging, battery and connection. The illustration is left-hand drive; front doors/windows are labeled
by driver/passenger role, while tires and heated seats are physical left/right.
Missing or unsupported readings display **Unknown**. Stale readings stay visible
in amber with a stale-data notice and the last successful vehicle update in the
header. Multiple vehicles are never combined. Select `SAIC_VIN` for a single
vehicle dashboard when an account contains more than one car.

The top timestamp is the stored vehicle observation time, shown as an updating
“ago” age. The browser reads cached status every ten seconds while visible; those
reads never poll or wake the car. **Refresh** requests a live poll, **Locate** on
the hood flashes lights, and **Climate** at the dash opens the automatic-climate
control. Its optional interior temperature override chooses the existing heating
routine, not the cabin target. These are all three service control endpoints;
other labels show status. Commands are disabled while one is running or the
service is unavailable, and errors/uncertain outcomes are never retried automatically.

The shell, stylesheets, scripts, favicon and vehicle image are public, contain no
account data, and are embedded in the executable. All telemetry and actions require
the token. The precompiled Tabler stylesheet is pinned to `@tabler/core` 1.7.5 and
stored with its MIT license in [src/dashboard/vendor](src/dashboard/vendor). The
vendored stylesheet drops the upstream source-map comment.
No JavaScript build step, external fonts, CDN, or runtime asset directory is needed.

Telemetry conversion follows the [upstream vehicle publisher](https://github.com/SAIC-iSmart-API/saic-python-mqtt-gateway/blob/master/src/status_publisher/vehicle/basic_vehicle_status.py):
tire units are 0.04 bar (0 and 255 are unavailable), and door-window flags require
valid mileage to exclude incomplete packets. Unknown status codes are omitted.
Parked status considers engine and handbrake; missing inputs do not imply parked.

## HTTP API

All API and control requests require `Authorization: Bearer <CLIMATE_API_TOKEN>`. Token digests
are compared in constant time. Responses include no-store, nosniff, no-referrer,
frame denial and restrictive Content Security Policy headers. Use a trusted TLS
reverse proxy for access beyond loopback; this listener speaks HTTP.

- `GET /api/status`: `connected` means login and account vehicle discovery
  succeeded; additive `ready` means a single authorized vehicle can accept
  requests. Neither guarantees telemetry is available or fresh. Existing readings
  retain `value`, `retained` and `fresh`, plus isolated `sources`.
- `POST /climate/start`: optional JSON object with only `temperature`. Numbers
  are Celsius; text supports decimal comma, signs and optional °C/°F. Without an
  override, the fresh interior temperature is used; an unavailable or stale reading
  means 503. This input selects heating behavior, not the A/C target temperature.
- `POST /refresh`: explicitly read live vehicle and charging state; return 202
  after the refreshed snapshot is stored, or 503 on failure. Available immediately
  after login and single-vehicle discovery, even with empty or stale telemetry and
  a future saved polling deadline. It uses the normal refresh scheduling path,
  preserving known parking age. Login, status and widget reads do not poll the car.
- `POST /locate`: flash lights only, never sound the horn; return 200 after API
  completion. Completion does not prove the physical vehicle action occurred.

Climate starts front defrost at <=4°C, otherwise A/C at 22°C using the client's
standard temperature profile. At <=6°C it then heats left seat, right seat and
steering wheel; at <=4°C it finally starts rear defrost. Each operation uses a
client command builder and waits for completion before the next. The two seats
are always independent commands. There are no status reads between commands.
After the complete successful batch, the service refreshes vehicle and charging
state once and updates the normal polling schedule from the observed activity.
A failed refresh marks telemetry stale and enters polling backoff, but the climate
request still succeeds and starts the preheating notification; commands are never
replayed. Failed command batches do not trigger a refresh. Locate leaves polling
deadlines unchanged. Controls are mutually exclusive.

Bodies are limited to 4096 bytes and a five-second read timeout. Transfer-Encoding,
malformed JSON, unknown keys and bodies on status/refresh/locate return 400.
Authentication failures return 401; authenticated requests to unknown routes
return 404. Unavailability, rejection, partial failure and uncertain outcomes
return 503 with redacted messages. Unavailable session/discovery, ambiguous
vehicle selection and missing fresh climate temperature have distinct errors.
Client operations have a 30-second deadline;
the HTTP control wait is bounded to 360 seconds. Cancelled queued requests are
skipped. An already submitted sequence may finish after the HTTP caller leaves.

Commands are never automatically replayed. A failed sequence stops immediately;
earlier commands may have succeeded. Check vehicle state before retrying an
unknown or partial outcome. Only full climate success starts the preheating
notification signal. Notify failures cannot change command success or repeat
vehicle commands.

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
  charging/plugged/driving/parked status, interior temperature and A/C.
  Cached values remain visible with `Last known / stale` when required readings
  are stale or disconnected. The widget is separate from the browser dashboard.
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

The build helper stages only Cargo manifests and source (including dashboard assets) from this repository
and the sibling Rust client, excluding credentials and other local files. The
staged build context also uses an allowlist. Run the helper from this checkout;
a bare `docker build .` does not contain the required sibling source.

```sh
./scripts/build-container.sh saic-rust-notify:local
docker run --rm --name saic-rust-notify \
  --env-file .env \
  --publish 127.0.0.1:8080:8080 \
  --mount type=volume,src=saic-polling,dst=/app/data \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  saic-rust-notify:local
```

To build for an Ubuntu amd64 host, transfer the image over SSH, and replace the
running container without publishing to a registry, create
`~/saic-rust-notify/.env` on the host first, then run from this checkout:

```sh
./scripts/deploy.sh user@server
```

The script uses the persistent Docker volume `saic-rust-notify-data`, publishes
HTTP on the host's loopback interface at port 8080, and leaves the host `.env`
outside the image. Configure the environment file for container use (in
particular `HTTP_HOST=0.0.0.0` and `HTTP_PORT=8080`).

The final Debian image runs as UID/GID 10001 and contains only the Rust executable
and base runtime libraries. The `/app/data` volume must persist across container
replacement; the image creates it with UID/GID 10001 ownership. TLS uses Rustls
and bundled trust roots; no Python or
OpenSSL runtime is needed. HTTP defaults to `0.0.0.0:8080` in the image; override
`HTTP_HOST=0.0.0.0` if your environment file supplies a loopback bind address.
Keep the HTTP port private or behind a TLS reverse proxy. Only authenticated,
explicitly requested controls send vehicle commands.

## Development

All tests use synthetic data. The session worker tests use a fake client boundary
and exercise discovery, polling, reconnect, ordered commands, unknown outcomes,
no replay, measurement conversion and freshness. HTTP and Notify tests cover
authentication, validation, notification payloads, lifecycle and errors. No tests
read runtime credentials, contact SAIC production, or operate a vehicle.

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
node tests/dashboard.test.mjs
node tests/dashboard-interactions.test.mjs
```

For a synthetic dashboard preview with no SAIC access:

```sh
cargo run --locked --offline --example dashboard_preview
```

Visit `http://127.0.0.1:8099/` with token `demo-token`. All displayed data and
actions in this preview are simulated. Never run the main service for UI testing.

Container verification uses synthetic credentials and an unreachable loopback
HTTPS API address, and never sends vehicle controls (requires Docker and curl):

```sh
./scripts/build-container.sh saic-rust-notify:local
./tests/container.sh saic-rust-notify:local
```

The Notify widget status shows `Updated at DD/MM/YYYY HH:MM` in Europe/Brussels
time (including daylight saving), prefixed with `Stale ·` when readings are stale.
It uses the stored vehicle observation timestamp, not notification delivery time.
Before telemetry arrives, it shows `No data yet`. Widget updates do not poll the car.
