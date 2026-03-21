# JawnScanner

A Prometheus exporter that tracks TSA security checkpoint wait times at the **Philadelphia International Airport (PHL)**.

JawnScanner fetches real-time wait time data from the PHL airport API and exposes it as Prometheus metrics, making it easy to build Grafana dashboards, set up alerts, and analyze historical trends.

## Checkpoints Tracked

| Checkpoint | Schedule (ET) |
|---|---|
| Terminal A-West | 05:00 - 22:15 |
| Terminal A-East | 04:15 - 22:15 |
| Terminal A-East TSA Pre | 04:15 - 18:30 |
| Terminal B | 03:30 - 21:30 |
| Terminal C | 04:15 - 20:00 |
| Terminal D/E | 03:00 - 22:30 |
| Terminal D/E TSA Pre | 03:45 - 20:00 |
| Terminal F | 04:30 - 21:15 |

## Metrics

| Metric | Type | Labels | Description |
|---|---|---|---|
| `phl_checkpoint_wait_minutes` | Gauge | `terminal` | Wait time in minutes (only present when checkpoint is open) |
| `phl_checkpoint_open` | Gauge | `terminal` | `1` if the checkpoint is open, `0` if closed |
| `phl_scrape_success` | Gauge | — | `1` if the PHL API responded successfully, `0` otherwise |

### Example Output

```
# HELP phl_checkpoint_open Whether the TSA checkpoint is currently open (1) or closed (0)
# TYPE phl_checkpoint_open gauge
phl_checkpoint_open{terminal="A-East"} 1
phl_checkpoint_open{terminal="A-East TSA Pre"} 1
phl_checkpoint_open{terminal="A-West"} 0
phl_checkpoint_open{terminal="B"} 1
phl_checkpoint_open{terminal="C"} 1
phl_checkpoint_open{terminal="D/E"} 1
phl_checkpoint_open{terminal="D/E TSA Pre"} 1
phl_checkpoint_open{terminal="F"} 0
# HELP phl_checkpoint_wait_minutes TSA checkpoint wait time in minutes
# TYPE phl_checkpoint_wait_minutes gauge
phl_checkpoint_wait_minutes{terminal="A-East"} 28.5
phl_checkpoint_wait_minutes{terminal="A-East TSA Pre"} 2
phl_checkpoint_wait_minutes{terminal="B"} 15
phl_checkpoint_wait_minutes{terminal="C"} 10
phl_checkpoint_wait_minutes{terminal="D/E"} 30
phl_checkpoint_wait_minutes{terminal="D/E TSA Pre"} 1.5
# HELP phl_scrape_success Whether the last scrape of PHL wait times was successful
# TYPE phl_scrape_success gauge
phl_scrape_success 1
```

## Quick Start

### Full Stack with Docker Compose

The easiest way to get everything running — JawnScanner, Prometheus, and Grafana:

```bash
docker compose up -d
```

Then open:
- **Grafana**: http://localhost:3000 (login: `admin` / `admin`)
- **Prometheus**: http://localhost:9090
- **JawnScanner metrics**: http://localhost:9101/metrics

To import the included dashboard, go to Grafana > Dashboards > Import > Upload `grafana/dashboard.json` and select your Prometheus datasource.

### Docker (standalone)

```bash
docker build -t jawnscanner .
docker run -d -p 9101:9101 --name jawnscanner jawnscanner
```

Verify it's working:

```bash
curl http://localhost:9101/metrics
```

### Pre-built Image

```bash
docker run -d -p 9101:9101 ghcr.io/tgrecojr/jawnscanner:latest
```

### From Source

Requires Rust 1.83+ (developed against 1.94).

```bash
cargo build --release
./target/release/jawnscanner
```

## Configuration

All configuration is via environment variables:

| Variable | Default | Description |
|---|---|---|
| `LISTEN_PORT` | `9101` | Port for the HTTP server |
| `PHL_API_URL` | `https://www.phl.org/phllivereach/metrics` | PHL wait times API endpoint |
| `PHL_PAGE_URL` | `https://www.phl.org/` | PHL homepage URL for status parsing |
| `RUST_LOG` | `info` | Log level (`debug`, `info`, `warn`, `error`) |

### Docker example with custom config

```bash
docker run -d -p 8080:8080 \
  -e LISTEN_PORT=8080 \
  -e RUST_LOG=debug \
  --name jawnscanner jawnscanner
```

## Prometheus Configuration

Add a scrape job to your `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: 'jawnscanner'
    scrape_interval: 60s
    static_configs:
      - targets: ['jawnscanner:9101']
```

A 60-second scrape interval is recommended. The PHL API data doesn't update more frequently than that.

## Grafana

### Dashboard

A pre-built dashboard is included at `grafana/dashboard.json`. Import it via Grafana UI (Dashboards > Import > Upload JSON file).

The dashboard includes:
- **Overview row**: Scrape status, open/closed checkpoint counts, max and average wait times
- **Current status**: Bar gauge of current wait times + table with open/closed status
- **Historical**: Wait times over time (time series) + open/closed timeline
- **Analysis**: Average and max wait per terminal over 24 hours, scrape health over time

### Useful Queries

**Current wait times (open checkpoints only):**
```promql
phl_checkpoint_wait_minutes
```

**Average wait time over the last hour:**
```promql
avg_over_time(phl_checkpoint_wait_minutes{terminal="B"}[1h])
```

**Which checkpoints are closed right now:**
```promql
phl_checkpoint_open == 0
```

**Max wait time across all open checkpoints:**
```promql
max(phl_checkpoint_wait_minutes)
```

## Endpoints

| Path | Description |
|---|---|
| `/metrics` | Prometheus metrics |
| `/health` | Health check (returns `OK`) |

## Development

```bash
# Run locally
cargo run

# Run tests
cargo test

# Lint
cargo clippy

# Format
cargo fmt
```

## How It Works

1. Prometheus scrapes the `/metrics` endpoint on the configured interval
2. JawnScanner fetches the PHL homepage HTML and the wait time API concurrently
3. Open/closed status is parsed from server-rendered CSS classes (`nu-open`/`nu-closed`) in the HTML — this reflects real-time operational closures
4. Open checkpoints get both a wait time and an `open=1` gauge
5. Closed checkpoints only get an `open=0` gauge (no wait time metric emitted)
6. If the homepage is unreachable, all checkpoints default to open
7. If the wait time API is unreachable, `phl_scrape_success` is set to `0`

## License

MIT
