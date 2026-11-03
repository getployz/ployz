//! [`ployz_store::stderr_log::install`] in a process of its own: what a host without
//! a `tracing` subscriber, as Ployz Cloud's native SDK, writes to stderr (#3786).

use std::process::Command;

const CHILD: &str = "PLOYZ_STORE_STDERR_LOG_CHILD";

#[test]
fn install_writes_each_store_warning_to_stderr_as_one_redacted_json_line() {
    if std::env::var_os(CHILD).is_some() {
        // Twice: a second install changes nothing.
        ployz_store::stderr_log::install();
        ployz_store::stderr_log::install();
        tracing::warn!(
            target: "ployz_store::branch::follow",
            branch = "00000000-0000-4000-8000-00000000000a",
            code = "unavailable",
            error = "canceling statement due to lock timeout\nDETAIL: fake-detail-3786",
            value = "plain-secret-3786",
            "couldn't seal values below a Branch"
        );
        tracing::info!(target: "ployz_store", "an info line");
        return;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "install_writes_each_store_warning_to_stderr_as_one_redacted_json_line",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env_remove(ployz_store::stderr_log::FILTER)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stderr = String::from_utf8(out.stderr).unwrap();
    // Every line stderr holds is one of the install's: libtest writes to stdout.
    let lines: Vec<serde_json::Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("{stderr}")))
        .collect();
    let [line] = lines.as_slice() else {
        panic!("{stderr}")
    };
    assert_eq!(line["level"], "WARN");
    assert_eq!(line["target"], "ployz_store::branch::follow");
    assert_eq!(line["message"], "couldn't seal values below a Branch");
    assert_eq!(
        line["fields"],
        serde_json::json!({
            "branch": "00000000-0000-4000-8000-00000000000a",
            "code": "unavailable",
            "error": "canceling statement due to lock timeout",
            "value": "[redacted]",
        })
    );
    assert!(!stderr.contains("plain-secret-3786"), "{stderr}");
    assert!(!stderr.contains("fake-detail-3786"), "{stderr}");
}
