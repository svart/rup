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
