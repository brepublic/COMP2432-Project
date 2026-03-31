# ProjA (Rust Sensor Pipeline + Dashboard)

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
thermo_count = 2
thermo_rates_per_sec = "50 50"
accel_count = 2
accel_rates_per_sec = "100 100"
force_count = 1
force_rates_per_sec = "75"

[buffer]
capacity = 5000
```

Notes:
- `*_count` controls how many sensors of each type are created.
- `*_rates_per_sec` uses a whitespace-separated list (in a TOML string), for example `"50 60 70"`.
- When `*_count` is larger than the number of rates provided, the last rate is reused for remaining sensors.
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

