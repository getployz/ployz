use std::process::{Command, Output};

fn ployz(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn version_flags_print_the_package_version() {
    for args in [["--version"].as_slice(), ["-V"].as_slice()] {
        let output = ployz(args);
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
        assert_eq!(stdout(&output), format!("{}\n", env!("CARGO_PKG_VERSION")));
    }
}

#[test]
fn version_command_prints_the_bare_package_version() {
    let output = ployz(&["version"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), format!("{}\n", env!("CARGO_PKG_VERSION")));
}
