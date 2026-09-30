//! CLI setup adapters around the shared Machine installer, and `setup agent`.
//!
//! Bootstrap scenarios run in-process in `provisioning::setup_tests`.

use std::{env, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn ssh_setup_with_no_install_skips_the_bootstrap() {
    let fixture = tempfile::tempdir().unwrap();
    let log = fixture.path().join("commands.log");
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("ssh"),
        "#!/bin/sh\nprintf 'ssh %s\\n' \"$*\" >> \"$LOG\"\nexit 23\n",
    );
    write_executable(
        &bin.join("scp"),
        "#!/bin/sh\nprintf 'scp %s\\n' \"$*\" >> \"$LOG\"\n",
    );
    let inherited = env::var_os("PATH").unwrap_or_default();
    let path = env::join_paths(std::iter::once(bin).chain(env::split_paths(&inherited))).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "server",
            "add",
            "--standalone",
            "deploy@2001:db8::1",
            "--no-install",
            "--yes",
            "--storage",
            "none",
            "--accepts-ingress=false",
            "--context",
            "setup-no-install",
        ])
        .env("PATH", path)
        .env("LOG", &log)
        .env("PLOYZ_CONFIG", fixture.path().join("config.yaml"))
        .output()
        .unwrap();

    assert!(!output.status.success(), "fake daemon must not accept RPC");
    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains("ssh "), "{log}");
    assert!(!log.contains("scp "), "{log}");
    assert!(!log.contains("'install'"), "{log}");
}

fn ployz_at(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .env("HOME", home)
        .env_remove("AI_AGENT")
        .output()
        .unwrap()
}

#[test]
fn setup_agent_installs_keeps_edits_and_help_reports_it() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(home.path().join(".claude")).unwrap();
    let help = || String::from_utf8(ployz_at(home.path(), &["--help"]).stdout).unwrap();
    assert!(help().contains("Settings catalog: `ployz schema --json`"));
    assert!(help().contains("no Ployz skill is installed"));

    let json = |args: &[&str]| -> serde_json::Value {
        let output = ployz_at(home.path(), args);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let outcomes = |result: &serde_json::Value| -> Vec<String> {
        result["locations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|location| location["outcome"].as_str().unwrap().to_owned())
            .collect()
    };
    let installed = json(&["setup", "agent", "--json"]);
    assert_eq!(outcomes(&installed), ["installed", "installed"]);
    let skill = home.path().join(".claude/skills/ployz/SKILL.md");
    assert!(fs::read_to_string(&skill).unwrap().contains("name: ployz"));
    assert!(help().contains("is current"));
    assert_eq!(
        outcomes(&json(&["setup", "agent", "--json"])),
        ["current", "current"]
    );

    fs::write(
        &skill,
        fs::read_to_string(&skill).unwrap() + "\nMy notes.\n",
    )
    .unwrap();
    assert!(help().contains("has local edits"));
    let kept = json(&["setup", "agent", "--json"]);
    assert_eq!(outcomes(&kept), ["current", "kept"]);
    assert_eq!(
        kept.get("next"),
        Some(&serde_json::json!("ployz setup agent --force"))
    );
    assert!(fs::read_to_string(&skill).unwrap().contains("My notes."));

    let forced = json(&["setup", "agent", "--force", "--json"]);
    assert_eq!(outcomes(&forced), ["current", "replaced"]);
    assert!(!fs::read_to_string(&skill).unwrap().contains("My notes."));
}
