use axum::{extract::State, http::header, response::IntoResponse, routing::get, Router};
use chrono::Timelike;
use chrono_tz::America::New_York;
use prometheus::{Encoder, GaugeVec, Opts, Registry, TextEncoder};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tracing::{info, warn};

const DEFAULT_PORT: u16 = 9101;
const DEFAULT_API_URL: &str = "https://www.phl.org/phllivereach/metrics";

struct Checkpoint {
    zone_id: u64,
    terminal: &'static str,
    open_hour: u32,
    open_min: u32,
    close_hour: u32,
    close_min: u32,
}

impl Checkpoint {
    const fn new(
        zone_id: u64,
        terminal: &'static str,
        open: (u32, u32),
        close: (u32, u32),
    ) -> Self {
        Self {
            zone_id,
            terminal,
            open_hour: open.0,
            open_min: open.1,
            close_hour: close.0,
            close_min: close.1,
        }
    }

    fn is_open(&self, hour: u32, minute: u32) -> bool {
        let now = hour * 60 + minute;
        let open = self.open_hour * 60 + self.open_min;
        let close = self.close_hour * 60 + self.close_min;
        if open == close {
            return false;
        }
        now >= open && now < close
    }
}

const CHECKPOINTS: &[Checkpoint] = &[
    Checkpoint::new(4377, "A-West", (5, 0), (22, 15)),
    Checkpoint::new(4368, "A-East", (4, 15), (22, 15)),
    Checkpoint::new(4386, "A-East TSA Pre", (4, 15), (18, 30)),
    Checkpoint::new(5047, "B", (3, 30), (21, 30)),
    Checkpoint::new(5052, "C", (4, 15), (20, 0)),
    Checkpoint::new(3971, "D/E", (3, 0), (22, 30)),
    Checkpoint::new(4126, "D/E TSA Pre", (3, 45), (20, 0)),
    Checkpoint::new(5068, "F", (4, 30), (21, 15)),
];

#[derive(Deserialize)]
struct PhlResponse {
    content: PhlContent,
}

#[derive(Deserialize)]
struct PhlContent {
    rows: Vec<PhlRow>,
}

#[derive(Deserialize)]
struct PhlRow(u64, f64, #[allow(dead_code)] WaitTimeRange);

#[derive(Deserialize)]
struct WaitTimeRange {
    #[allow(dead_code)]
    lower_bound: f64,
    #[allow(dead_code)]
    upper_bound: f64,
}

#[derive(Clone)]
struct AppState {
    client: Client,
    api_url: String,
}

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    let registry = Registry::new();

    let wait_gauge = GaugeVec::new(
        Opts::new(
            "phl_checkpoint_wait_minutes",
            "TSA checkpoint wait time in minutes",
        ),
        &["terminal"],
    )
    .unwrap();

    let open_gauge = GaugeVec::new(
        Opts::new(
            "phl_checkpoint_open",
            "Whether the TSA checkpoint is currently open (1) or closed (0)",
        ),
        &["terminal"],
    )
    .unwrap();

    let scrape_success = prometheus::Gauge::new(
        "phl_scrape_success",
        "Whether the last scrape of PHL wait times was successful",
    )
    .unwrap();

    registry.register(Box::new(wait_gauge.clone())).unwrap();
    registry.register(Box::new(open_gauge.clone())).unwrap();
    registry.register(Box::new(scrape_success.clone())).unwrap();

    let now = chrono::Utc::now().with_timezone(&New_York);
    let hour = now.hour();
    let minute = now.minute();

    let wait_times: HashMap<u64, f64> = match state.client.get(&state.api_url).send().await {
        Ok(resp) => match resp.json::<PhlResponse>().await {
            Ok(data) => {
                scrape_success.set(1.0);
                data.content.rows.iter().map(|r| (r.0, r.1)).collect()
            }
            Err(e) => {
                warn!("Failed to parse PHL API response: {}", e);
                scrape_success.set(0.0);
                HashMap::new()
            }
        },
        Err(e) => {
            warn!("Failed to fetch PHL API: {}", e);
            scrape_success.set(0.0);
            HashMap::new()
        }
    };

    for cp in CHECKPOINTS {
        let is_open = cp.is_open(hour, minute);
        open_gauge
            .with_label_values(&[cp.terminal])
            .set(if is_open { 1.0 } else { 0.0 });

        if is_open {
            if let Some(&wait) = wait_times.get(&cp.zone_id) {
                wait_gauge.with_label_values(&[cp.terminal]).set(wait);
            }
        }
    }

    let encoder = TextEncoder::new();
    let metric_families = registry.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();

    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        buffer,
    )
}

async fn health_handler() -> &'static str {
    "OK"
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let port: u16 = std::env::var("LISTEN_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let api_url = std::env::var("PHL_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string());

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client");

    let state = AppState { client, api_url };

    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/health", get(health_handler))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("JawnScanner listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_checkpoint(open: (u32, u32), close: (u32, u32)) -> Checkpoint {
        Checkpoint::new(1, "Test", open, close)
    }

    fn make_app(api_url: &str) -> Router {
        let state = AppState {
            client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            api_url: api_url.to_string(),
        };
        Router::new()
            .route("/metrics", get(metrics_handler))
            .route("/health", get(health_handler))
            .with_state(state)
    }

    #[test]
    fn is_open_during_hours() {
        let cp = test_checkpoint((5, 0), (22, 0));
        assert!(cp.is_open(12, 0));
    }

    #[test]
    fn is_open_at_exact_open_time() {
        let cp = test_checkpoint((5, 0), (22, 0));
        assert!(cp.is_open(5, 0));
    }

    #[test]
    fn is_closed_one_minute_before_open() {
        let cp = test_checkpoint((5, 0), (22, 0));
        assert!(!cp.is_open(4, 59));
    }

    #[test]
    fn is_closed_at_exact_close_time() {
        let cp = test_checkpoint((5, 0), (22, 0));
        assert!(!cp.is_open(22, 0));
    }

    #[test]
    fn is_closed_after_hours() {
        let cp = test_checkpoint((5, 0), (22, 0));
        assert!(!cp.is_open(23, 30));
    }

    #[test]
    fn is_closed_when_open_equals_close() {
        let cp = test_checkpoint((5, 0), (5, 0));
        assert!(!cp.is_open(5, 0));
    }

    #[test]
    fn is_open_with_non_zero_minutes() {
        let cp = test_checkpoint((4, 15), (18, 30));
        assert!(cp.is_open(4, 15));
        assert!(!cp.is_open(4, 14));
        assert!(cp.is_open(18, 29));
        assert!(!cp.is_open(18, 30));
    }

    #[test]
    fn all_checkpoints_defined() {
        assert_eq!(CHECKPOINTS.len(), 8);
    }

    #[test]
    fn deserialize_phl_response() {
        let json = r#"{
            "result": {"success": true, "httpCode": 200},
            "content": {
                "columns": [],
                "rows": [
                    [4377, 15.5, {"lower_bound": 13, "upper_bound": 18}],
                    [4368, 28.0, {"lower_bound": 25, "upper_bound": 30}]
                ]
            }
        }"#;
        let resp: PhlResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.rows.len(), 2);
        assert_eq!(resp.content.rows[0].0, 4377);
        assert!((resp.content.rows[0].1 - 15.5).abs() < f64::EPSILON);
        assert_eq!(resp.content.rows[1].0, 4368);
    }

    #[test]
    fn deserialize_phl_response_empty_rows() {
        let json = r#"{
            "result": {"success": true, "httpCode": 200},
            "content": {
                "columns": [],
                "rows": []
            }
        }"#;
        let resp: PhlResponse = serde_json::from_str(json).unwrap();
        assert!(resp.content.rows.is_empty());
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let app = make_app("http://localhost:0/fake");
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap();
        assert_eq!(&body[..], b"OK");
    }

    #[tokio::test]
    async fn metrics_endpoint_with_mock_api() {
        let mock_server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/phllivereach/metrics"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "result": {"success": true, "httpCode": 200},
                    "content": {
                        "columns": [],
                        "rows": [
                            [4377, 15.5, {"lower_bound": 13, "upper_bound": 18}],
                            [4368, 28.0, {"lower_bound": 25, "upper_bound": 30}],
                            [5047, 10.0, {"lower_bound": 8, "upper_bound": 13}]
                        ]
                    }
                })),
            )
            .mount(&mock_server)
            .await;

        let app = make_app(&format!("{}/phllivereach/metrics", mock_server.uri()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();

        assert!(body_str.contains("phl_checkpoint_open"));
        assert!(body_str.contains("phl_scrape_success 1"));
        assert!(body_str.contains("phl_checkpoint_wait_minutes"));
    }

    #[tokio::test]
    async fn metrics_endpoint_api_failure_still_returns_200() {
        let app = make_app("http://127.0.0.1:1/unreachable");
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();

        assert!(body_str.contains("phl_scrape_success 0"));
        assert!(body_str.contains("phl_checkpoint_open"));
    }

    #[tokio::test]
    async fn metrics_content_type_is_prometheus_format() {
        let app = make_app("http://127.0.0.1:1/unreachable");
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/plain"));
        assert!(content_type.contains("0.0.4"));
    }
}
