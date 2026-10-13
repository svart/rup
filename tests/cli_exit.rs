use std::process::Command;

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
