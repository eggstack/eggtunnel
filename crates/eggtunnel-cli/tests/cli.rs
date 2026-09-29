//! CLI command integration tests: `version`, human/JSON `check`,
//! override precedence, startup JSON events, and secret redaction over
//! real process stdout/stderr.

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::Duration,
};

fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_eggtunnel").into()
}

fn write_config(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "eggtunnel-cli-it-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn client_config(token_env: &str) -> String {
    format!(
        r#"mode = "client"
transport = "tcp_tls"
server_addr = "127.0.0.1:9443"
tls_server_name = "localhost"
token_env = "{token_env}"

[[services]]
id = 1
name = "web"
target_host = "127.0.0.1"
target_port = 8080
bind_port = 0
"#
    )
}

#[test]
fn version_prints_the_crate_version() {
    let output = Command::new(bin()).arg("version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout.trim(),
        format!("eggtunnel {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn check_human_accepts_a_valid_client_config() {
    let dir = temp_dir("check-human");
    let path = write_config(&dir, "client.toml", &client_config("EGGTUNNEL_IT_TOKEN"));
    let output = Command::new(bin())
        .arg("check")
        .arg(&path)
        .env("EGGTUNNEL_IT_TOKEN", "it-token-value")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("structurally valid"));
}

#[test]
fn check_json_is_stable_redacted_and_versioned() {
    let dir = temp_dir("check-json");
    let path = write_config(
        &dir,
        "client.toml",
        &client_config("EGGTUNNEL_IT_JSON_TOKEN"),
    );
    let output = Command::new(bin())
        .arg("check")
        .arg("--json")
        .arg(&path)
        .env("EGGTUNNEL_IT_JSON_TOKEN", "json-token-secret")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["schema"], "eggtunnel.check/v1");
    assert_eq!(value["ok"], true);
    assert_eq!(value["mode"], "client");
    assert_eq!(value["transport"], "tcp_tls");
    assert_eq!(value["services"], 1);
    assert!(!stdout.contains("json-token-secret"));
}

#[test]
fn check_json_reports_invalid_transport_with_a_stable_category() {
    let dir = temp_dir("check-json-bad");
    let mut body = client_config("EGGTUNNEL_IT_BAD_TOKEN");
    body = body.replace("tcp_tls", "wss");
    let path = write_config(&dir, "client.toml", &body);
    let output = Command::new(bin())
        .arg("check")
        .arg("--json")
        .arg(&path)
        .env("EGGTUNNEL_IT_BAD_TOKEN", "bad-token")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["category"], "transport");
}

#[test]
fn cli_overrides_win_over_toml_fields() {
    let dir = temp_dir("override");
    // TOML carries an invalid endpoint; only a valid CLI override rescues it.
    let mut body = client_config("EGGTUNNEL_IT_OV_TOKEN");
    body = body.replace("127.0.0.1:9443", "not an endpoint");
    let path = write_config(&dir, "client.toml", &body);
    let mut child = Command::new(bin())
        .arg("client")
        .arg(&path)
        .arg("--json")
        .arg("--server-addr")
        .arg("127.0.0.1:10443")
        .env("EGGTUNNEL_IT_OV_TOKEN", "ov-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut started = false;
    while std::time::Instant::now() < deadline {
        match lines.next() {
            Some(Ok(line)) if line.contains("\"event\":\"startup\"") => {
                started = true;
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    child.kill().ok();
    child.wait().ok();
    assert!(
        started,
        "valid override did not rescue invalid TOML endpoint"
    );
    // An invalid override fails even when TOML is valid.
    let path = write_config(&dir, "good.toml", &client_config("EGGTUNNEL_IT_OV_TOKEN"));
    let output = Command::new(bin())
        .arg("client")
        .arg(&path)
        .arg("--server-addr")
        .arg("not-an-endpoint")
        .env("EGGTUNNEL_IT_OV_TOKEN", "ov-token")
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn client_startup_json_events_are_redacted_and_well_formed() {
    let dir = temp_dir("client-events");
    let path = write_config(&dir, "client.toml", &client_config("EGGTUNNEL_IT_EV_TOKEN"));
    let mut child = Command::new(bin())
        .arg("client")
        .arg(&path)
        .arg("--json")
        .env("EGGTUNNEL_IT_EV_TOKEN", "event-token-secret")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while events.is_empty() && std::time::Instant::now() < deadline {
        match lines.next() {
            Some(Ok(line)) if !line.trim().is_empty() => events.push(line),
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    child.kill().ok();
    let output = child.wait_with_output().unwrap();
    assert!(
        !events.is_empty(),
        "no startup event; stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_str(&events[0]).unwrap();
    assert_eq!(value["schema"], "eggtunnel.events/v1");
    assert_eq!(value["event"], "startup");
    assert_eq!(value["mode"], "client");
    assert_eq!(value["transport"], "tcp_tls");
    assert_eq!(value["services"], 1);
    for line in &events {
        assert!(
            !line.contains("event-token-secret"),
            "secret leaked: {line}"
        );
    }
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("event-token-secret"));
}

#[test]
fn server_startup_json_reports_listening_and_binds() {
    let dir = temp_dir("server-events");
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            key.to_str().unwrap(),
            "-out",
            cert.to_str().unwrap(),
            "-days",
            "1",
            "-nodes",
            "-subj",
            "/CN=localhost",
        ])
        .output()
        .unwrap();
    assert!(status.status.success());
    let body = format!(
        r#"mode = "server"
transport = "tcp_tls"
listen_addr = "127.0.0.1:0"
tls_cert = "{}"
tls_key = "{}"
token_env = "EGGTUNNEL_IT_SRV_TOKEN"
"#,
        cert.to_string_lossy(),
        key.to_string_lossy()
    );
    let path = write_config(&dir, "server.toml", &body);
    let mut child = Command::new(bin())
        .arg("server")
        .arg(&path)
        .arg("--json")
        .env("EGGTUNNEL_IT_SRV_TOKEN", "srv-token-secret")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while events.len() < 2 && std::time::Instant::now() < deadline {
        match lines.next() {
            Some(Ok(line)) if !line.trim().is_empty() => events.push(line),
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    child.kill().ok();
    let output = child.wait_with_output().unwrap();
    assert!(
        events.len() >= 2,
        "stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let startup: serde_json::Value = serde_json::from_str(&events[0]).unwrap();
    assert_eq!(startup["event"], "startup");
    assert_eq!(startup["mode"], "server");
    let listening: serde_json::Value = serde_json::from_str(&events[1]).unwrap();
    assert_eq!(listening["event"], "server_listening");
    assert!(
        listening["addr"]
            .as_str()
            .unwrap()
            .starts_with("127.0.0.1:")
    );
    for line in &events {
        assert!(!line.contains("srv-token-secret"), "secret leaked: {line}");
    }
}
