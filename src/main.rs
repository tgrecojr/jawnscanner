use axum::{extract::State, http::header, response::IntoResponse, routing::get, Router};
use prometheus::{Encoder, GaugeVec, Opts, Registry, TextEncoder};
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tracing::{info, warn};

const DEFAULT_PORT: u16 = 9101;
const DEFAULT_API_URL: &str = "https://www.phl.org/phllivereach/metrics";
const DEFAULT_PAGE_URL: &str = "https://www.phl.org/";

struct CheckpointDef {
    zone_id: u64,
    terminal: &'static str,
}

impl CheckpointDef {
    const fn new(zone_id: u64, terminal: &'static str) -> Self {
        Self { zone_id, terminal }
    }
}

const CHECKPOINTS: &[CheckpointDef] = &[
    CheckpointDef::new(4377, "A-West"),
    CheckpointDef::new(4368, "A-East"),
    CheckpointDef::new(4386, "A-East TSA Pre"),
    CheckpointDef::new(5047, "B"),
    CheckpointDef::new(5052, "C"),
    CheckpointDef::new(3971, "D/E"),
    CheckpointDef::new(4126, "D/E TSA Pre"),
    CheckpointDef::new(5068, "F"),
];

/// Parses the PHL homepage HTML for checkpoint open/closed status.
/// PHL server-renders `class="status nu-open"` or `class="status nu-closed"`
/// for each checkpoint in the Security Status section.
fn parse_checkpoint_statuses(html: &str) -> HashMap<String, bool> {
    let mut statuses = HashMap::new();
    let name_re = Regex::new(r"<strong>([\w/\-]+)</strong>(\s*TSA Pre)?").unwrap();

    for block in html.split("garage with-msg") {
        if let Some(cap) = name_re.captures(block) {
            let base = &cap[1];
            let is_pre = cap.get(2).is_some();
            let terminal = if is_pre {
                format!("{} TSA Pre", base)
            } else {
                base.to_string()
            };

            let is_open = block.contains("nu-open");
            let is_closed = block.contains("nu-closed");

            if is_open || is_closed {
                statuses.insert(terminal, is_open);
            }
        }
    }

    statuses
}

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
    page_url: String,
}

/// Attempts to fetch and parse wait time data from the PHL API.
async fn fetch_api_wait_times(client: &Client, api_url: &str) -> Result<HashMap<u64, f64>, String> {
    let resp = client
        .get(api_url)
        .send()
        .await
        .map_err(|e| format!("fetch failed: {}", e))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("status={} body read failed: {}", status, e))?;
    let data: PhlResponse = serde_json::from_str(&body).map_err(|e| {
        let truncated: String = body.chars().take(500).collect();
        format!("status={} parse error={} body={}", status, e, truncated)
    })?;
    Ok(data.content.rows.iter().map(|r| (r.0, r.1)).collect())
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

    // Fetch page HTML and wait times concurrently
    let (page_result, api_result) = tokio::join!(
        state.client.get(&state.page_url).send(),
        fetch_api_wait_times(&state.client, &state.api_url),
    );

    let checkpoint_statuses: HashMap<String, bool> = match page_result {
        Ok(resp) => match resp.text().await {
            Ok(html) => {
                let parsed = parse_checkpoint_statuses(&html);
                if parsed.is_empty() {
                    warn!("Failed to parse checkpoint statuses from HTML, defaulting to open");
                }
                parsed
            }
            Err(e) => {
                warn!("Failed to read PHL page response: {}", e);
                HashMap::new()
            }
        },
        Err(e) => {
            warn!("Failed to fetch PHL page: {}", e);
            HashMap::new()
        }
    };

    let wait_times: HashMap<u64, f64> = match api_result {
        Ok(data) => {
            scrape_success.set(1.0);
            data
        }
        Err(first_err) => {
            info!(error = %first_err, "PHL API fetch failed, retrying in 1s");
            tokio::time::sleep(Duration::from_secs(1)).await;
            match fetch_api_wait_times(&state.client, &state.api_url).await {
                Ok(data) => {
                    info!("PHL API retry succeeded");
                    scrape_success.set(1.0);
                    data
                }
                Err(retry_err) => {
                    warn!(error = %retry_err, "PHL API retry also failed");
                    scrape_success.set(0.0);
                    HashMap::new()
                }
            }
        }
    };

    for cp in CHECKPOINTS {
        // Use HTML status if available, default to open if HTML parsing failed
        let is_open = checkpoint_statuses
            .get(cp.terminal)
            .copied()
            .unwrap_or(true);

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
    let page_url = std::env::var("PHL_PAGE_URL").unwrap_or_else(|_| DEFAULT_PAGE_URL.to_string());

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client");

    let state = AppState {
        client,
        api_url,
        page_url,
    };

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

    fn make_app(api_url: &str, page_url: &str) -> Router {
        let state = AppState {
            client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            api_url: api_url.to_string(),
            page_url: page_url.to_string(),
        };
        Router::new()
            .route("/metrics", get(metrics_handler))
            .route("/health", get(health_handler))
            .with_state(state)
    }

    fn sample_html(statuses: &[(&str, bool)]) -> String {
        let mut html = String::new();
        for (name, is_open) in statuses {
            let (terminal_html, status_html) = if name.contains("TSA Pre") {
                let base = name.replace(" TSA Pre", "");
                let term = format!(
                    r#"<a class="term-link">Terminal <strong>{}</strong> TSA Pre✓</a>"#,
                    base
                );
                let status = if *is_open {
                    r#"<div class="status nu-open"><span>3</span> mins</div>"#.to_string()
                } else {
                    r#"<span class="status nu-closed">Closed</span>"#.to_string()
                };
                (term, status)
            } else {
                let term = format!(
                    r#"<a class="term-link">Terminal <strong>{}</strong></a>"#,
                    name
                );
                let status = if *is_open {
                    r#"<div class="status nu-open"><span>3</span> mins</div>"#.to_string()
                } else {
                    r#"<span class="status nu-closed">Closed</span>"#.to_string()
                };
                (term, status)
            };
            html.push_str(&format!(
                r#"<div class="garage with-msg"><div class="title-full"><div class="gtitle">{}</div><div class="gfull">{}</div></div></div>"#,
                terminal_html, status_html
            ));
        }
        html
    }

    #[test]
    fn parse_statuses_mixed_open_closed() {
        let html = sample_html(&[
            ("A-West", false),
            ("A-East", true),
            ("A-East TSA Pre", true),
            ("B", true),
            ("C", false),
            ("D/E", true),
            ("D/E TSA Pre", true),
            ("F", false),
        ]);
        let statuses = parse_checkpoint_statuses(&html);

        assert_eq!(statuses.len(), 8);
        assert_eq!(statuses["A-West"], false);
        assert_eq!(statuses["A-East"], true);
        assert_eq!(statuses["A-East TSA Pre"], true);
        assert_eq!(statuses["B"], true);
        assert_eq!(statuses["C"], false);
        assert_eq!(statuses["D/E"], true);
        assert_eq!(statuses["D/E TSA Pre"], true);
        assert_eq!(statuses["F"], false);
    }

    #[test]
    fn parse_statuses_all_closed() {
        let html = sample_html(&[
            ("A-West", false),
            ("A-East", false),
            ("B", false),
            ("C", false),
            ("D/E", false),
            ("F", false),
        ]);
        let statuses = parse_checkpoint_statuses(&html);
        assert!(statuses.values().all(|&v| !v));
    }

    #[test]
    fn parse_statuses_all_open() {
        let html = sample_html(&[
            ("A-West", true),
            ("A-East", true),
            ("B", true),
            ("C", true),
            ("D/E", true),
            ("F", true),
        ]);
        let statuses = parse_checkpoint_statuses(&html);
        assert!(statuses.values().all(|&v| v));
    }

    #[test]
    fn parse_statuses_empty_on_bad_input() {
        assert!(parse_checkpoint_statuses("").is_empty());
        assert!(parse_checkpoint_statuses("garbage html").is_empty());
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
        let app = make_app("http://localhost:0/fake", "http://localhost:0/fake");
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

        let html_body = sample_html(&[
            ("A-West", false),
            ("A-East", true),
            ("A-East TSA Pre", true),
            ("B", true),
            ("C", false),
            ("D/E", true),
            ("D/E TSA Pre", true),
            ("F", false),
        ]);

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(html_body))
            .mount(&mock_server)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/phllivereach/metrics"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "result": {"success": true, "httpCode": 200},
                    "content": {
                        "columns": [],
                        "rows": [
                            [4377, 15.5, {"lower_bound": 13, "upper_bound": 18}],
                            [4368, 3.0, {"lower_bound": 1, "upper_bound": 5}],
                            [5047, 14.0, {"lower_bound": 12, "upper_bound": 16}],
                            [5052, 2.0, {"lower_bound": 1, "upper_bound": 4}]
                        ]
                    }
                })),
            )
            .mount(&mock_server)
            .await;

        let app = make_app(
            &format!("{}/phllivereach/metrics", mock_server.uri()),
            &mock_server.uri(),
        );
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

        assert!(body_str.contains("phl_scrape_success 1"));
        // Closed checkpoints
        assert!(body_str.contains(r#"phl_checkpoint_open{terminal="A-West"} 0"#));
        assert!(body_str.contains(r#"phl_checkpoint_open{terminal="C"} 0"#));
        assert!(body_str.contains(r#"phl_checkpoint_open{terminal="F"} 0"#));
        // Closed checkpoints should NOT have wait times
        assert!(!body_str.contains(r#"phl_checkpoint_wait_minutes{terminal="A-West"}"#));
        assert!(!body_str.contains(r#"phl_checkpoint_wait_minutes{terminal="C"}"#));
        assert!(!body_str.contains(r#"phl_checkpoint_wait_minutes{terminal="F"}"#));
        // Open checkpoints
        assert!(body_str.contains(r#"phl_checkpoint_open{terminal="A-East"} 1"#));
        assert!(body_str.contains(r#"phl_checkpoint_open{terminal="B"} 1"#));
        assert!(body_str.contains(r#"phl_checkpoint_wait_minutes{terminal="A-East"} 3"#));
        assert!(body_str.contains(r#"phl_checkpoint_wait_minutes{terminal="B"} 14"#));
    }

    #[tokio::test]
    async fn metrics_endpoint_api_failure_still_returns_200() {
        let app = make_app(
            "http://127.0.0.1:1/unreachable",
            "http://127.0.0.1:1/unreachable",
        );
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
    async fn metrics_endpoint_retries_on_empty_api_response() {
        let mock_server = wiremock::MockServer::start().await;

        let html_body = sample_html(&[("A-East", true)]);

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(html_body))
            .mount(&mock_server)
            .await;

        // Valid response (lower priority — registered first)
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/phllivereach/metrics"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "result": {"success": true, "httpCode": 200},
                    "content": {
                        "columns": [],
                        "rows": [
                            [4368, 5.0, {"lower_bound": 3, "upper_bound": 7}]
                        ]
                    }
                })),
            )
            .mount(&mock_server)
            .await;

        // Empty {} response (higher priority — registered last), consumed after 1 hit
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/phllivereach/metrics"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("{}"))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        let app = make_app(
            &format!("{}/phllivereach/metrics", mock_server.uri()),
            &mock_server.uri(),
        );
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

        // Retry should have succeeded
        assert!(body_str.contains("phl_scrape_success 1"));
        assert!(body_str.contains(r#"phl_checkpoint_wait_minutes{terminal="A-East"} 5"#));
    }

    #[tokio::test]
    async fn metrics_content_type_is_prometheus_format() {
        let app = make_app(
            "http://127.0.0.1:1/unreachable",
            "http://127.0.0.1:1/unreachable",
        );
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
