use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn empty_workspace_exits_gracefully() {
    let dir = TempDir::new().unwrap();

    Command::cargo_bin("crow")
        .unwrap()
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("No verification candidates"))
        .stdout(predicates::str::contains("NoVerifierAvailable"));
}
