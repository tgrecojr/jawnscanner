//! Behavioral security test for the Grafana admin login (VULN-001).
//!
//! `deployment_config.rs` asserts the *shape* of the checked-in configuration.
//! This test asserts the *behavior* that configuration produces: it stands up
//! the pinned Grafana image wired exactly as `docker-compose.yml` wires it and
//! replays the reported attack -- HTTP basic auth as `admin`/`admin` against
//! the admin API.
//!
//! Requires Docker, so it is `#[ignore]`d and does not run in a plain
//! `cargo test`. Run it explicitly:
//!
//! ```text
//! cargo test --test grafana_auth -- --ignored
//! ```

use std::process::Command;

/// The password a real operator would supply via `GRAFANA_ADMIN_PASSWORD`.
const OPERATOR_SECRET: &str = "correct-horse-battery-staple-9271";
const HOST_PORT: u16 = 34117;
const CONTAINER: &str = "jawnscanner_vuln001_auth_test";

fn repo_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name)
}

/// Pull the pinned Grafana image out of docker-compose.yml so this test keeps
/// tracking the digest the stack actually deploys.
fn grafana_image() -> String {
    let compose = std::fs::read_to_string(repo_path("docker-compose.yml")).expect("read compose");
    compose
        .lines()
        .filter_map(|line| line.trim().strip_prefix("image:"))
        .map(str::trim)
        .find(|image| image.starts_with("grafana/grafana"))
        .expect("docker-compose.yml must pin a grafana/grafana image")
        .to_string()
}

/// Resolve the admin password the way an actual `docker compose up` would.
///
/// This is what makes the test discriminating rather than a staged demo: it
/// does not assume a good password, it takes whatever the checked-in compose
/// file yields. If the compose file ships a literal, the deployment gets that
/// literal -- and the attack below then succeeds, failing this test. If the
/// compose file defers to an environment variable, the operator supplies one.
fn effective_admin_password() -> String {
    let compose = std::fs::read_to_string(repo_path("docker-compose.yml")).expect("read compose");

    let configured = compose
        .lines()
        .map(|line| line.trim().trim_start_matches('-').trim())
        .find_map(|entry| entry.strip_prefix("GF_SECURITY_ADMIN_PASSWORD="))
        .map(str::trim)
        .expect("docker-compose.yml must configure GF_SECURITY_ADMIN_PASSWORD");

    if configured.starts_with("${") {
        // Deferred to the environment: the operator picks the value.
        OPERATOR_SECRET.to_string()
    } else {
        // Shipped in version control: every deployer gets exactly this.
        configured.to_string()
    }
}

/// Removes the container even if an assertion panics.
struct Container;

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", CONTAINER])
            .output();
    }
}

impl Container {
    fn start(image: &str, admin_password: &str) -> Self {
        let _ = Command::new("docker")
            .args(["rm", "-f", CONTAINER])
            .output();

        let admin_secret = format!("GF_SECURITY_ADMIN_PASSWORD={admin_password}");
        let out = Command::new("docker")
            .args([
                "run",
                "-d",
                "--name",
                CONTAINER,
                "-p",
                &format!("127.0.0.1:{HOST_PORT}:3000"),
                "-e",
                &admin_secret,
                "-e",
                "GF_USERS_ALLOW_SIGN_UP=false",
                image,
            ])
            .output()
            .expect("failed to invoke docker; is it installed and running?");
        assert!(
            out.status.success(),
            "docker run failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Container
    }
}

/// GET /api/org with the supplied basic-auth pair.
///
/// Returns `None` while the container is still starting and the port is not
/// yet accepting connections, so the readiness loop can keep polling.
async fn try_get_org_status(client: &reqwest::Client, user: &str, pass: &str) -> Option<u16> {
    client
        .get(format!("http://127.0.0.1:{HOST_PORT}/api/org"))
        .basic_auth(user, Some(pass))
        .send()
        .await
        .ok()
        .map(|resp| resp.status().as_u16())
}

/// Same, but for use after readiness has been established, where a transport
/// error is a genuine failure rather than a not-yet-listening container.
async fn get_org_status(client: &reqwest::Client, user: &str, pass: &str) -> u16 {
    try_get_org_status(client, user, pass)
        .await
        .expect("request to Grafana failed after it had already become ready")
}

#[tokio::test]
#[ignore = "requires Docker; run with: cargo test --test grafana_auth -- --ignored"]
async fn reported_default_login_is_rejected_by_the_admin_api() {
    let image = grafana_image();
    let deployed_password = effective_admin_password();
    let _container = Container::start(&image, &deployed_password);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("build client");

    // Wait for Grafana to finish booting, using the operator's real password.
    let mut ready = false;
    for _ in 0..90 {
        if try_get_org_status(&client, "admin", &deployed_password).await == Some(200) {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    assert!(ready, "Grafana never became ready; cannot run the attack");

    // The attack exactly as reported: the credential that used to ship in
    // version control. Pre-fix this returned 200 with an admin session.
    let attack = get_org_status(&client, "admin", "admin").await;
    assert_eq!(
        attack, 401,
        "the reported default login was accepted by the admin API (HTTP {attack}); \
         the admin password must come from GRAFANA_ADMIN_PASSWORD, never a shipped default"
    );

    // The same credential must not be able to perform a state-changing write
    // either -- pre-fix this created a datasource and returned 200.
    let write = client
        .post(format!("http://127.0.0.1:{HOST_PORT}/api/datasources"))
        .basic_auth("admin", Some("admin"))
        .json(&serde_json::json!({
            "name": "vuln001-regression-probe",
            "type": "prometheus",
            "url": "http://prometheus:9090",
            "access": "proxy",
        }))
        .send()
        .await
        .expect("datasource request failed")
        .status()
        .as_u16();
    assert_eq!(
        write, 401,
        "the reported default login could still write to /api/datasources (HTTP {write})"
    );

    // Fail-closed control: unauthenticated access is rejected...
    let anon = client
        .get(format!("http://127.0.0.1:{HOST_PORT}/api/org"))
        .send()
        .await
        .expect("anonymous request failed")
        .status()
        .as_u16();
    assert_eq!(
        anon, 401,
        "unauthenticated access was accepted (HTTP {anon})"
    );

    // ...while the operator's configured password still works, so the fix
    // blocks the attack without breaking the legitimate login.
    let operator = get_org_status(&client, "admin", &deployed_password).await;
    assert_eq!(
        operator, 200,
        "the operator's configured password was rejected (HTTP {operator}); \
         the fix must not break legitimate access"
    );
}
