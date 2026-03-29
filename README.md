# COMP2432 - ProjA (Rust Sensor Pipeline + Dashboard)  

Semester project of COMP2432 Operating Systems, the Hong Kong Polytechnic University.  
This project simulates sensor streams, aggregates them in the backend, persists aggregated frames to `./data`, and serves a live dashboard.

## Quick Start

1. Start the gateway (which also starts the dashboard):
   - `cargo run -p gateway`
2. Open the dashboard:
   - `http://127.0.0.1:5800/`

## Configuration (config.toml)

The gateway reads runtime configuration from a TOML file.

### Config file path

The config path is controlled by environment variable:
- `GATEWAY_CONFIG` (optional): path to config file (default: `./config.toml`)

### Supported sections

Example `config.toml`:

```toml
[sensors]
thermo_1_rate_per_sec = 50
thermo_2_rate_per_sec = 50
accel_1_rate_per_sec = 100
accel_2_rate_per_sec = 100
force_1_rate_per_sec = 75

[buffer]
capacity = 5000
```

Notes:
- `*_rate_per_sec` controls the simulated generation rate per sensor (events per second).
- `buffer.capacity` controls the bounded in-memory queue capacity used between sensor readers and the aggregation workers.

## Environment Variables (debug / feature switches)

Boolean values: any of `1`, `true`, `TRUE`, `True` are treated as enabled.

- `GATEWAY_FAIL_ON_FULL` (optional, default: `false`)
  - If enabled, the process will `panic!` when `SharedBuffer` detects it is full during `push()`.
  - Intended for debugging / validating that backpressure never gets into overflow territory.
- `GATEWAY_DEBUG_BUFFER` (optional, default: `false`)
  - If enabled, the gateway prints per-second buffer stats:
    - current `len`, plus `pushed+ /s` and `popped+ /s` deltas.

## Dashboard Pages

- `/latest` : latest aggregated frames (auto refresh)
- `/stats` : overall statistics (auto refresh)
- `/sensor` : list of available sensors
- `/sensor/<id>` : per-sensor live statistics + anomalies (auto refresh)

JSON APIs (used by the pages):
- `/api/latest`
- `/api/stats`
- `/api/sensor/<id>`

