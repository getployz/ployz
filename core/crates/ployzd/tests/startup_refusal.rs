use std::{fs, process::Command};

#[test]
fn a_start_that_fails_at_its_first_step_clears_the_last_refusal() {
    let root = std::env::temp_dir().join(format!("ployzd-startup-refusal-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let run = root.join("run");
    fs::create_dir_all(&run).unwrap();
    fs::write(
        run.join("refused"),
        "refusing to change the retained WireGuard interface\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["--socket", run.join("ployz.sock").to_str().unwrap()])
        .args(["--data-dir", root.join("data").to_str().unwrap()])
        .args(["--log-level", "ployzd=verbos"])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "a bad log level must fail the start"
    );
    assert!(
        !run.join("refused").exists(),
        "this start did not refuse, so no refusal may stand: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}
