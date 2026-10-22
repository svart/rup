use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Kills the server process even when an assertion fails.
struct ServerProcess(Child);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_tcp_server() -> (ServerProcess, String) {
    let address = TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .unwrap()
        .to_string();
    let server = ServerProcess(
        Command::new(env!("CARGO_BIN_EXE_rup"))
            .args(["-p", "tcp", "server", &address])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn rup server"),
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(&address).is_err() {
        assert!(Instant::now() < deadline, "rup server did not start");
        sleep(Duration::from_millis(10));
    }
    (server, address)
}

#[test]
fn invalid_client_target_exits_with_failure() {
    let output = Command::new(env!("CARGO_BIN_EXE_rup"))
        .args(["-p", "udp", "localhost"])
        .output()
        .expect("run rup");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires a port"));
}

#[test]
fn help_exits_successfully_on_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_rup"))
        .arg("--help")
        .output()
        .expect("run rup");

    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: rup"));
}

#[test]
fn version_exits_successfully_on_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_rup"))
        .arg("--version")
        .output()
        .expect("run rup");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!("rup ", env!("CARGO_PKG_VERSION"), "\n")
    );
}

#[test]
fn invalid_option_exits_with_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_rup"))
        .args(["--no-such-option", "127.0.0.1"])
        .output()
        .expect("run rup");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--no-such-option"));
}

#[test]
fn client_pings_rup_server_and_writes_jsonl() {
    let (_server, address) = spawn_tcp_server();

    let output = Command::new(env!("CARGO_BIN_EXE_rup"))
        .args([
            "-p", "tcp", "-n", "2", "-i", "10", "--output", "jsonl", &address,
        ])
        .output()
        .expect("run rup");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let kinds = records
        .iter()
        .map(|record| record["record"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(kinds, ["metadata", "reply", "reply", "summary"]);
    assert_eq!(records[1]["seq"], 0);
    assert_eq!(records[2]["seq"], 1);
    assert_eq!(records[3]["sent"], 2);
    assert_eq!(records[3]["received"], 2);
}
