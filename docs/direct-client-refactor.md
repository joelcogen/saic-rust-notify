# Direct client refactor

The former runtime required the SAIC MQTT Gateway and a broker for discovery,
telemetry and commands. The service now owns session recovery and periodic reads
through `../saic-rust-client`; neither external process is required. The gateway
and `../saic-mqtt-service` remain behavioral references, not dependencies.

The HTTP and Notify contracts retain their existing reading names and heating
thresholds. Direct commands use client builders and bounded event continuation.
The worker serializes reads and entire command sequences. It never retries a
control or inserts a status read between sequence steps. Controls leave the fixed per-vehicle polling deadlines unchanged. Durable parking
epochs and deadlines in `data/polling.json` survive reconnects and restarts; see
the README for staged parking, charging and backoff rules. The client remains responsible for
SAIC authentication, cryptography, transport and command payloads.

The Rust client's response models intentionally expose raw measurements. This
service's presentation adapter applies the existing unit conversions needed by
Notify. Conversion evidence is the client's pinned Python schema at
`SAIC-iSmart-API/saic-python-client-ng`, commit
`5ffd4e2bbb501d1afbfb272d1f546c0dd2030d0e`, paths
`src/saic_ismart_client_ng/api/vehicle/schema.py` and
`src/saic_ismart_client_ng/api/vehicle_charging/schema.py`, together with the local
gateway's `src/status_publisher/vehicle/basic_vehicle_status.py`,
`src/status_publisher/charge/chrg_mgmt_data.py` and `src/utils.py`:

- BMS SOC is tenths of a percent; target codes 1–7 mean 40–100%.
- Battery current is raw × 0.05 − 1000 A, voltage raw × 0.25 V; power is kW.
- Remaining charging time is minutes converted to seconds.
- Charging states 1, 3, 10 and 12 indicate charging; unknown codes remain unknown.
- Temperatures are already Celsius; 87 and values outside −127–127 are invalid.
- Climate codes 0/1/2/5 mean off/ventilation/on/front defrost.

Unlike the previous stream, a successful direct poll replaces its vehicle snapshot:
missing data cannot inherit freshness from a newer timestamp. Vehicle state must
have a recent API timestamp. Charging management has no timestamp and uses receipt
age. Failures leave cached values visible but stale. Reauthentication restores session
and request readiness independently of telemetry; failed readings remain stale
until a successful refresh. Optional VIN selection
is verified against account discovery before polling or accepting controls.

Offline tests use synthetic typed responses and a fake session boundary. Runtime
production access and physical vehicle behavior have not been tested.
