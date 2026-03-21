# JawnScanner

## Overview
Prometheus exporter that tracks TSA security checkpoint wait times at PHL (Philadelphia International Airport). Scrapes the PHL airport API and exposes metrics for Prometheus to collect, enabling Grafana dashboards and alerting.

## Tech Stack
- Language: Rust
- HTTP Server: axum
- HTTP Client: reqwest (rustls)
- Metrics: prometheus crate
- Runtime: tokio
- Container: distroless (gcr.io/distroless/cc-debian12:nonroot)

## Commands
- `cargo build --release` — Build optimized binary
- `cargo run` — Run locally (serves on :9101)
- `cargo test` — Run tests
- `cargo clippy` — Lint
- `cargo fmt --check` — Check formatting
- `docker build -t jawnscanner .` — Build container image
- `docker run -p 9101:9101 jawnscanner` — Run container

## Architecture
Single-binary Prometheus exporter with two endpoints:
- `GET /metrics` — Prometheus metrics endpoint
- `GET /health` — Health check

On each scrape, the exporter:
1. Fetches the checkpoint schedule from PHL's `wait-api.js` (dynamic, reflects operational closures)
2. Fetches wait time data from the PHL API (`/phllivereach/metrics`)
3. Determines open/closed status using the dynamic schedule (Eastern Time)
4. Exposes `phl_checkpoint_open` (1/0) and `phl_checkpoint_wait_minutes` (only when open)
5. Includes `phl_scrape_success` to indicate API fetch health
6. Falls back to hardcoded default schedules if `wait-api.js` is unreachable

Eight checkpoints are tracked: A-West, A-East, A-East TSA Pre, B, C, D/E, D/E TSA Pre, F.

## Environment Variables
- `LISTEN_PORT` — HTTP server port (default: 9101)
- `PHL_API_URL` — PHL wait times API URL (default: https://www.phl.org/phllivereach/metrics)
- `PHL_JS_URL` — PHL schedule JS URL (default: https://www.phl.org/modules/custom/phl_wait_api/js/wait-api.js)
- `RUST_LOG` — Log level filter (default: info)
