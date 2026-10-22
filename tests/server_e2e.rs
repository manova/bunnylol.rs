/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn unique_test_dir(test_name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "bunnylol-e2e-{}-{}-{}",
        test_name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    dir
}

fn config_path(xdg_dir: &Path) -> PathBuf {
    xdg_dir.join("bunnylol/config.toml")
}

fn write_config(xdg_dir: &Path, default_search: &str, port: u16) {
    fs::create_dir_all(xdg_dir.join("bunnylol")).expect("create config dir");
    fs::write(
        config_path(xdg_dir),
        format!(
            r#"default_search = "{default_search}"

[history]
enabled = false

[server]
port = {port}
address = "127.0.0.1"
log_level = "critical"
"#
        ),
    )
    .expect("write config");
}

fn write_invalid_config(xdg_dir: &Path) {
    fs::write(config_path(xdg_dir), "default_search = [\n").expect("write invalid config");
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind test port")
        .local_addr()
        .expect("read test port")
        .port()
}

struct ServerProcess {
    child: Child,
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server(xdg_dir: &Path, port: u16) -> ServerProcess {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("bunnylol"));
    let child = command
        .env("XDG_CONFIG_HOME", xdg_dir)
        .arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .arg("--address")
        .arg("127.0.0.1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn bunnylol server");

    ServerProcess { child }
}

fn http_get(port: u16, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn wait_for_server(server: &mut ServerProcess, port: u16) {
    for _ in 0..50 {
        if let Some(status) = server.child.try_wait().expect("check server status") {
            panic!("server exited before becoming ready: {status}");
        }

        if let Ok(response) = http_get(port, "/health")
            && response.contains("ok")
        {
            return;
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    panic!("server did not become ready");
}

fn redirect_location(response: &str) -> String {
    assert!(
        response.starts_with("HTTP/1.1 303") || response.starts_with("HTTP/1.1 302"),
        "expected redirect response, got:\n{response}"
    );

    response
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("location")
                .then(|| value.trim().to_string())
        })
        .expect("redirect response should include Location header")
}

fn assert_redirect_starts_with(port: u16, expected_prefix: &str) {
    let response = http_get(port, "/?cmd=e2e-reload-query").expect("request redirect");
    let location = redirect_location(&response);
    assert!(
        location.starts_with(expected_prefix),
        "expected redirect to start with {expected_prefix}, got {location}"
    );
}

#[test]
#[cfg(feature = "server")]
fn test_server_reloads_config_mtime_and_keeps_last_valid_config() {
    let xdg_dir = unique_test_dir("config-reload");
    let port = free_port();
    write_config(&xdg_dir, "google", port);

    let mut server = spawn_server(&xdg_dir, port);
    wait_for_server(&mut server, port);

    assert_redirect_starts_with(port, "https://www.google.com/search?q=");

    std::thread::sleep(Duration::from_millis(1100));
    write_config(&xdg_dir, "ddg", port);
    assert_redirect_starts_with(port, "https://duckduckgo.com/?q=");

    std::thread::sleep(Duration::from_millis(1100));
    write_invalid_config(&xdg_dir);
    assert_redirect_starts_with(port, "https://duckduckgo.com/?q=");

    fs::remove_dir_all(&xdg_dir).ok();
}

#[test]
#[cfg(all(feature = "server", feature = "cli"))]
fn test_pattern_cli_server_parity_and_reload_recovery() {
    let xdg_dir = unique_test_dir("patterns-parity");
    let port = free_port();
    let fixture = r#"
default_search = 'ddg'
[history]
enabled = false
[user_bindings]
work = { url = 'https://work.example/' }
alias = { command = 'ENG-100' }
gh = { command = 'gh manova', override = true }
[[pattern_bindings]]
name = 'ticket'
pattern = '(?P<ticket>ENG-[0-9]+)'
url = 'https://tracker.example/{ticket}'
[[pattern_bindings]]
name = 'overlap'
pattern = '(?P<ticket>ENG-[0-9]+)'
url = 'https://overlap.example/{ticket}'
[[pattern_bindings]]
name = 'pr'
pattern = '#(?P<number>[0-9]+)'
url = 'https://github.com/example/repo/pull/{number}'
[[pattern_bindings]]
name = 'encoding'
pattern = 'value:(?P<value>.+)'
url = 'https://example.com/item/{value}?q={value}'
[[pattern_bindings]]
name = 'command-collision'
pattern = '(?P<command>gh|work|ig|r/rust|\$AAPL|alias)'
url = 'https://wrong.example/{command}'
[[pattern_bindings]]
name = 'unicode-path'
pattern = 'snow-(?P<id>[0-9]+)'
url = 'https://example.com/雪/{id}'
[[pattern_bindings]]
name = 'idn'
pattern = 'idn-(?P<id>[0-9]+)'
url = 'https://例え.com/{id}'
[[pattern_bindings]]
name = 'uri-punctuation'
pattern = 'pipe-(?P<id>[0-9]+)'
url = 'https://example.com/a|b^{id}?q=|^#|^'
[[pattern_bindings]]
name = 'verbose-comment'
pattern = '(?x) verbose-(?P<id>[0-9]+) # ticket'
url = 'https://example.com/{id}'
[[pattern_bindings]]
name = 'fragment-hash'
pattern = 'fragment-(?P<id>[0-9]+)'
url = 'https://example.com/{id}#a#b'
"#;
    fs::create_dir_all(xdg_dir.join("bunnylol")).unwrap();
    fs::write(config_path(&xdg_dir), fixture).unwrap();
    let mut server = spawn_server(&xdg_dir, port);
    wait_for_server(&mut server, port);

    for input in [
        "ENG-1234",
        "#9164",
        "look up ENG-1234",
        "ENG-1234 ",
        "gh",
        "work",
        "ig",
        "r/rust",
        "$AAPL",
        "alias",
        "value://evil.test/a?x=1&y=雪#frag%",
        "snow-42",
        "idn-42",
        "pipe-42",
        "verbose-42",
        "verbose-42 extra words",
        "fragment-42",
    ] {
        let output = Command::new(assert_cmd::cargo::cargo_bin!("bunnylol"))
            .env("XDG_CONFIG_HOME", &xdg_dir)
            .args(["--dry-run", input])
            .output()
            .unwrap();
        assert!(output.status.success());
        let cli_url = String::from_utf8(output.stdout).unwrap().trim().to_string();
        let encoded = bunnylol::utils::url_encoding::encode_url_special_char(input);
        let server_url = redirect_location(&http_get(port, &format!("/?cmd={encoded}")).unwrap());
        assert_eq!(server_url, cli_url, "CLI/server mismatch for {input:?}");
        if input == "ENG-1234" {
            assert_eq!(server_url, "https://tracker.example/ENG-1234");
        }
        if input == "alias" {
            assert_eq!(server_url, "https://tracker.example/ENG-100");
        }
    }
    let page = http_get(port, "/").unwrap();
    assert!(page.contains("Pattern Bindings"));
    assert!(page.contains("ticket"));
    assert!(page.contains("tracker.example"));

    // Force distinct mtimes without depending on the filesystem's clock resolution.
    let rewrite = |text: &str, tick: u64| {
        fs::write(config_path(&xdg_dir), text).unwrap();
        fs::File::options()
            .write(true)
            .open(config_path(&xdg_dir))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(tick))
            .unwrap();
    };
    let updated = fixture.replace("tracker.example", "updated.example");
    rewrite(&updated, 2);
    let ticket_url = || redirect_location(&http_get(port, "/?cmd=ENG-1234").unwrap());
    assert_eq!(ticket_url(), "https://updated.example/ENG-1234");
    assert!(http_get(port, "/").unwrap().contains("updated.example"));
    for (i, invalid) in [
        updated.replace(
            "(?P<ticket>ENG-[0-9]+)",
            "(?P<ticket>ENG-[0-9]+))|(?:OCT-[0-9]+",
        ),
        updated.replace("(?P<ticket>ENG-[0-9]+)", "["),
        updated.replace("https://updated.example/{ticket}", "https://{ticket}/"),
        updated.replace(
            "https://updated.example/{ticket}",
            "https://updated.example/{missing}",
        ),
    ]
    .iter()
    .enumerate()
    {
        rewrite(invalid, 4 + i as u64);
        assert_eq!(ticket_url(), "https://updated.example/ENG-1234");
        let page = http_get(port, "/").unwrap();
        assert!(page.contains("updated.example"));
    }
    let recovered = fixture.replace("tracker.example", "recovered.example");
    rewrite(&recovered, 9);
    assert_eq!(ticket_url(), "https://recovered.example/ENG-1234");
    rewrite("[history]\nenabled = false\n", 10);
    assert!(ticket_url().starts_with("https://www.google.com/search?q="));
    assert!(!http_get(port, "/").unwrap().contains("Pattern Bindings"));
    drop(server);
    fs::remove_dir_all(xdg_dir).unwrap();
}
