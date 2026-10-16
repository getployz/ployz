//! The loop the unit starts must run the ployzd `--install` chose.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use clap::{ArgMatches, Command};

use super::{command, loop_ployzd, loop_start};
use crate::checkin::{Host, Paths, Probe, unit};

fn exec_start(text: &str) -> Vec<String> {
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .unwrap();
    line.split('"')
        .enumerate()
        .flat_map(|(at, part)| {
            if at % 2 == 1 {
                vec![part.to_owned()]
            } else {
                part.split_whitespace().map(str::to_owned).collect()
            }
        })
        .collect()
}

/// A ployzd that answers `version` with `version`.
fn daemon(path: &Path, version: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, format!("#!/bin/sh\necho {version}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn temp_paths(dir: &tempfile::TempDir) -> Paths {
    Paths {
        checkin_dir: dir.path().join("checkin"),
        prepare_state: dir.path().join("prepare/state.json"),
        token_file: dir.path().join("fleet-token"),
    }
}

fn pinned(path: &Path) -> ArgMatches {
    command(Command::new("checkin"))
        .no_binary_name(true)
        .try_get_matches_from(["--ployzd", path.to_str().unwrap()])
        .unwrap()
}

#[tokio::test]
async fn the_unit_starts_the_loop_with_the_ployzd_install_chose() {
    let dir = tempfile::tempdir().unwrap();
    let chosen = dir.path().join("checkin/ployzd");
    daemon(&chosen, env!("CARGO_PKG_VERSION"));
    let chosen = chosen.as_path();
    let text = unit::checkin(
        Path::new("/usr/local/bin/ployz"),
        chosen,
        Path::new("/etc/ployz/fleet-token"),
        "https://cloud.example",
    )
    .unwrap();
    let args = exec_start(&text);
    assert_eq!(args.get(1..3).unwrap(), ["server", "checkin"]);
    let matches = command(Command::new("checkin"))
        .no_binary_name(true)
        .try_get_matches_from(args.get(3..).unwrap())
        .unwrap();
    let paths = Paths::system(PathBuf::from("/etc/ployz/fleet-token"));
    assert_eq!(loop_ployzd(&matches, &paths, None).await.unwrap(), chosen);
}

#[tokio::test]
async fn a_pinned_ployzd_that_is_gone_or_stale_is_found_again_without_a_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    let bin = dir.path().join("bin");
    let on_path = bin.join("ployzd");
    let copy = paths.checkin_dir.join("ployzd");
    let stale = dir.path().join("old/ployzd");
    daemon(&stale, "0.0.1");
    let search = bin.clone().into_os_string();
    let missing = dir.path().join("gone/ployzd");

    for pin in [&stale, &missing] {
        let error = loop_ployzd(&pinned(pin), &paths, Some(&search))
            .await
            .unwrap_err();
        assert!(
            format!("{error:?}").contains("Run ployz server checkin --install"),
            "{error:?}"
        );
    }
    assert!(!copy.exists(), "the loop never fetches");

    daemon(&copy, env!("CARGO_PKG_VERSION"));
    for pin in [&stale, &missing] {
        assert_eq!(
            loop_ployzd(&pinned(pin), &paths, Some(&search))
                .await
                .unwrap(),
            copy
        );
    }
    daemon(&on_path, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        loop_ployzd(&pinned(&stale), &paths, Some(&search))
            .await
            .unwrap(),
        on_path
    );
}

#[derive(Default)]
struct Stopper {
    disables: AtomicU32,
}

impl Host for Stopper {
    async fn probe(&self) -> Result<Probe, String> {
        unreachable!("the loop never starts")
    }

    fn hostname(&self) -> String {
        unreachable!("the loop never starts")
    }

    async fn prepare_running(&self) -> Result<bool, String> {
        unreachable!("the loop never starts")
    }

    async fn start_prepare(&self) -> Result<(), String> {
        unreachable!("the loop never starts")
    }

    async fn disable_loop(&self) -> Result<(), String> {
        self.disables.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn a_loop_without_a_token_ends_before_it_looks_for_ployzd() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    let search = dir.path().join("bin").into_os_string();
    let stopper = Stopper::default();
    let started = loop_start(
        &stopper,
        &pinned(&dir.path().join("gone/ployzd")),
        &paths,
        Some(&search),
    )
    .await;
    assert!(matches!(started, Ok(None)), "{:?}", started.err());
    assert_eq!(stopper.disables.load(Ordering::SeqCst), 1);
}
