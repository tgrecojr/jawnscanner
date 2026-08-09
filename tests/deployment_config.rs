//! Security regression tests for checked-in deployment configuration.
//!
//! These assert properties of the files that ship in version control, so a
//! future edit that reintroduces a shipped password or an off-host admin
//! port fails `cargo test` rather than reaching a deployment.

fn repo_file(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Yields each `KEY=VALUE` entry of every `environment:` list in the compose file.
fn compose_env_entries(compose: &str) -> Vec<(String, String)> {
    compose
        .lines()
        .map(|line| line.trim())
        .filter_map(|line| line.strip_prefix('-'))
        .map(|entry| entry.trim().trim_matches(['"', '\'']))
        .filter_map(|entry| entry.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

/// Yields `(host_side, container_port)` for each published port mapping.
///
/// `"127.0.0.1:3000:3000"` -> `("127.0.0.1:3000", "3000")`
/// `"3000:3000"`           -> `("3000", "3000")`
fn compose_port_mappings(compose: &str) -> Vec<(String, String)> {
    compose
        .lines()
        .map(|line| line.trim())
        .filter_map(|line| line.strip_prefix('-'))
        .map(|entry| entry.trim().trim_matches(['"', '\'']))
        .filter(|entry| !entry.contains('=') && entry.contains(':'))
        .filter_map(|entry| entry.rsplit_once(':'))
        .map(|(host, container)| (host.to_string(), container.to_string()))
        .collect()
}

/// The password must never be a literal in version control. Anyone who can
/// read the repository would otherwise recover a working admin login.
#[test]
fn grafana_admin_password_is_not_hardcoded() {
    let compose = repo_file("docker-compose.yml");
    let mut checked = 0;

    for (key, value) in compose_env_entries(&compose) {
        if key != "GF_SECURITY_ADMIN_PASSWORD" {
            continue;
        }
        checked += 1;
        assert!(
            value.starts_with("${"),
            "docker-compose.yml ships a literal Grafana admin password ({value:?}); \
             source it from an environment variable instead, and give that \
             reference a `:?` suffix so an unset value aborts the deploy"
        );
        assert!(
            value.contains(":?"),
            "the Grafana admin password is env-indirected but has no `:?` required-variable \
             guard ({value:?}); without it an unset variable silently becomes an empty password"
        );
    }

    assert_eq!(
        checked, 1,
        "expected exactly one GF_SECURITY_ADMIN_PASSWORD entry in docker-compose.yml, found \
         {checked} -- if the service was renamed or removed, update this test deliberately"
    );
}

/// Binding the admin UI to every host interface is what turned a weak
/// password into a remotely reachable one.
#[test]
fn grafana_admin_ui_is_not_published_on_all_interfaces() {
    let compose = repo_file("docker-compose.yml");

    for (host_side, container_port) in compose_port_mappings(&compose) {
        if container_port != "3000" {
            continue;
        }
        assert!(
            host_side.starts_with("127.0.0.1:") || host_side.starts_with("localhost:"),
            "the Grafana admin UI is published as {host_side:?}:{container_port:?}, which binds \
             every host interface; bind it to loopback (\"127.0.0.1:3000:3000\") so the admin \
             interface is not reachable off-host"
        );
    }
}

/// The README published the login pair alongside the URL, so it was
/// discoverable without even reading the compose file.
#[test]
fn readme_does_not_publish_grafana_login() {
    let readme = repo_file("README.md");

    assert!(
        !readme.contains("`admin` / `admin`"),
        "README.md publishes the default Grafana login pair; document the \
         GRAFANA_ADMIN_PASSWORD .env variable instead of a literal password"
    );
}
