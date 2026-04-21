#![allow(clippy::unwrap_used)]
use assert_cmd::Command;

#[test]
fn help_exits_gracefully() {
    Command::cargo_bin("crow")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
}
