# ProjA — Rust sensor pipeline + dashboard

Cargo workspace that **simulates sensor streams**, **buffers** readings, **aggregates** them on a fixed window (default 1s), **persists** JSON frames under `./data`, and serves a **live HTTP dashboard** (Salvo + HTML templates).

## Requirements

- **Rust** with **edition 2024** support (toolchain new enough for the workspace).

## Workspace layout

| Crate | Role |
|--------|------|
| `gateway` | Binary: loads config, spawns sensors and aggregation workers, embeds the dashboard thread. |
| `dashboard` | Library: HTTP server, pages, REST APIs, in-memory cache/telemetry. |
| `sensor_sim` | Thermometer, accelerometer, force sensor simulators and traits. |
| `common_models` | Shared serde types (frames, stats, anomalies, telemetry). |
| `os_lib` | Low-level helpers (e.g. queue primitives). |

## Quick start

```bash
cargo run -p gateway
```

Open the UI (use the host/port from your config’s `[dashboard]` section, or defaults below):

- **Dashboard:** `http://127.0.0.1:5800/` (or your configured `addr`)

## Configuration (`config.toml`)

Path: set **`GATEWAY_CONFIG`** to a TOML file, or default **`./config.toml`**. If the file is missing or invalid, the gateway logs a warning and uses built-in defaults.

### Example

```toml
[sensors]
thermo_count = 2
thermo_rates_per_sec = "50 30"
accel_count = 2
accel_rates_per_sec = "50 50"
force_count = 1
force_rates_per_sec = "50"

[buffer]
# SharedBuffer capacity (number of readings between producers and consumers).
capacity = 5000

[sensor_internal_buffer]
# Per-sensor internal ring (sensor_sim): usable slots and “near full” ratio for warnings/telemetry.
usable_capacity = 127
near_full_ratio = 0.85

[dashboard]
addr = "127.0.0.1:5800"
```

### Sensor fields

- `*_count` — how many sensors of that type are spawned.
- `*_rates_per_sec` — whitespace-separated rates in a TOML string, e.g. `"50 60 70"`. If there are fewer rates than sensors, the **last** rate is reused.

### Dashboard bind address

- **`[dashboard].addr`** — listen address. If this is **empty** after trimming, the gateway uses **`DASHBOARD_ADDR`**, then falls back to **`0.0.0.0:5800`**.

## Environment variables

Booleans: `1`, `true`, `TRUE`, `True` enable the flag.

| Variable | Default | Meaning |
|----------|---------|---------|
| `GATEWAY_CONFIG` | `./config.toml` | Path to gateway TOML. |
| `GATEWAY_FAIL_ON_FULL` | `false` | If set, **panic** when the shared buffer cannot accept a push (debug). |
| `GATEWAY_DEBUG_BUFFER` | `false` | Per-second **stdout** stats: buffer length, pushed/s popped/s deltas. |
| `DASHBOARD_ADDR` | (only if `[dashboard].addr` empty) | Fallback bind address for the dashboard. |

## Data on disk

- Aggregated frames are written as **`./data/frame_<window_end>_<frame_id>.json`** using write-to-temp-then-rename for atomic updates.

## Dashboard pages

| Path | Description |
|------|-------------|
| `/` | Index / overview |
| `/latest` | Latest aggregated frames (auto refresh) |
| `/stats` | Overall statistics (auto refresh) |
| `/sensor` | Sensor list |
| `/sensor/<id>` | Per-sensor stats + anomalies (auto refresh) |
| `/shutdown` | Shutdown UI |

## JSON APIs

| Method | Path | Notes |
|--------|------|--------|
| GET | `/api/latest` | Latest frames |
| GET | `/api/stats` | Aggregated stats |
| GET | `/api/buffer` | Internal buffer / per-sensor queue telemetry |
| GET | `/api/throughput` | Shared buffer throughput counters |
| GET | `/api/range` | Range query over stored frames (**can scan many JSON files** — avoid high QPS in benchmarks) |
| GET | `/api/sensor/<id>` | Per-sensor API |
| POST | `/api/shutdown` | Request dashboard shutdown |
| GET | `/api/shutdown/status` | Shutdown flag |
| GET | `/api/shutdown/watch` | Wait until shutting down |

## Shutdown

- **Ctrl+C** triggers graceful shutdown: stop sensor readers, stop aggregation, request dashboard shutdown, join the dashboard thread.
- The **shutdown** API routes coordinate an orderly HTTP server stop (see dashboard implementation for exact semantics).

## Benchmarks

The **`bench_runner`** binary drives scenarios from a TOML file (default **`benchmark.toml`**):

```bash
cargo run -p gateway --bin bench_runner -- path/to/scenarios.toml
```

See comments inside `benchmark.toml` for HTTP load, ingest thresholds, and warnings about expensive endpoints.

## Build release (optional)

```bash
cargo build -p gateway --release
```

Binary path for benchmarks defaults to `target/release/gateway` in `benchmark.toml`’s `[global]` section.
