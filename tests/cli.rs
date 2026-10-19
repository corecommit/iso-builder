//! End-to-end coverage of the headless CLI path.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iso-builder-cli-{}-{}", tag, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn dry_run_on_fake_iso_exits_zero() {
    let dir = temp_dir("dry-run");
    let iso = dir.join("fake.iso");
    fs::write(&iso, b"not a real ISO; --dry-run never opens it").unwrap();
    let iso = fs::canonicalize(&iso).unwrap();

    // Run from the empty temp dir to prove the binary does not need the repo
    // checkout (or its `config/` directory) as its working directory.
    let output = Command::new(env!("CARGO_BIN_EXE_iso-builder"))
        .args(["--dry-run", "--iso"])
        .arg(&iso)
        .current_dir(&dir)
        .output()
        .expect("failed to run iso-builder");

    assert_eq!(
        output.status.code(),
        Some(0),
        "exit status: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("#!/bin/bash\n"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains(&*iso.to_string_lossy()),
        "the generated script should embed the source ISO path"
    );
    assert!(
        !dir.join("run-builder.sh").exists(),
        "--dry-run must not write run-builder.sh"
    );

    let _ = fs::remove_dir_all(&dir);
}
