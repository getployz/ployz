//! ZFS preparation contracts with isolated host command adapters.

use super::*;
use crate::installer::test_support::{fixture, run_contract_child_with_environment, write_script};
use std::{
    env,
    ffi::OsString,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn storage_device_parsing_preserves_default_and_named_targets() {
    for value in ["none", "zfs", "zfs:/dev/vdb", "zfs:/dev/disk/by-id/spare"] {
        assert_eq!(StoragePreparation::parse(value).unwrap().to_string(), value);
    }
    for value in ["zfs:", "zfs:vdb", "none:/dev/vdb", "other"] {
        assert!(StoragePreparation::parse(value).is_err(), "{value}");
    }
}

#[test]
fn storage_device_pool_requires_exactly_the_requested_backing() {
    let status = "  pool: ployz-device\n state: ONLINE\nconfig:\n\tNAME STATE\n\tployz-device ONLINE\n\t  /dev/vdb ONLINE\n";
    assert!(device_pool_matches(status, Path::new("/dev/vdb")));
    assert!(!device_pool_matches(status, Path::new("/dev/vdc")));
    assert!(!device_pool_matches(
        &format!("{status}\t  /dev/vdc ONLINE\n"),
        Path::new("/dev/vdb")
    ));
    assert!(device_pool_matches(
        "ployz-device ONLINE\n  null ONLINE\n",
        Path::new("/dev/null")
    ));
    assert!(!device_pool_matches(
        "ployz-device ONLINE\n  mirror-0 ONLINE\n    /dev/vdb ONLINE\n    123456 UNAVAIL\n",
        Path::new("/dev/vdb")
    ));
}

#[test]
fn storage_refuses_missing_or_non_block_device_before_preparing_zfs() {
    let root = tempfile::tempdir().unwrap();
    let regular = root.path().join("regular");
    fs::write(&regular, "keep this data").unwrap();
    for device in [regular.clone(), root.path().join("missing")] {
        assert!(super::super::assess::empty_device(&device).is_err());
    }
    assert_eq!(fs::read_to_string(regular).unwrap(), "keep this data");
}

#[test]
fn named_storage_refuses_a_missing_device_before_querying_an_unloaded_zfs_module() {
    const CASE: &str = "PLOYZ_DEVICE_UNLOADED_CONTRACT";
    if env::var_os(CASE).is_some() {
        let root = contract_root();
        let storage =
            StoragePreparation::parse(format!("zfs:{}", root.join("missing").display())).unwrap();
        let failure = prepare_storage(&storage, &InstallPaths::at(&root)).unwrap_err();
        assert!(
            matches!(
                failure,
                Error::Io {
                    stage: "resolve ZFS device",
                    ..
                }
            ),
            "{failure}"
        );
        assert!(!root.join("queried-zpool").exists());
        fs::write(root.join("child-completed"), "missing-device").unwrap();
        return;
    }
    let root = fixture("device-unloaded");
    let commands = root.path().join("commands");
    fs::create_dir(&commands).unwrap();
    write_script(
        &commands.join("zpool"),
        r#": > "$PLOYZ_INSTALLER_CONTRACT_ROOT/queried-zpool"; echo 'module unavailable' >&2; exit 1"#,
    );
    run_contract_child_with_environment(
        "installer::storage::tests::named_storage_refuses_a_missing_device_before_querying_an_unloaded_zfs_module",
        root.path(),
        CASE.into(),
        "1".into(),
        "missing-device",
        [],
    );
}

#[test]
fn named_storage_accepts_only_validated_absolute_device_paths() {
    assert!(ployz_core::ZfsDevice::parse("vdb").is_err());
    assert!(ployz_core::ZfsDevice::parse("").is_err());
    let device = ployz_core::ZfsDevice::parse("/dev/disk/by-id/spare").unwrap();
    assert_eq!(device.as_path(), Path::new("/dev/disk/by-id/spare"));
}

#[test]
fn named_storage_retries_only_the_same_unambiguous_device_pool() {
    const CASE: &str = "PLOYZ_EXISTING_DEVICE_POOL_CONTRACT";
    if let Ok(case) = env::var(CASE) {
        let root = contract_root();
        let paths = InstallPaths::at(&root);
        let storage =
            StoragePreparation::parse(format!("zfs:{}", root.join("device").display())).unwrap();
        let result = prepare_storage(&storage, &paths);
        if case == "same" {
            result.unwrap();
            assert!(paths.device_pool_marker.exists());
        } else {
            let failure = result.unwrap_err().to_string();
            let expected = match case.as_str() {
                "foreign" => "single Machine Pool",
                "ambiguous" => "ambiguous",
                "other-device" => "not backed solely",
                _ => unreachable!(),
            };
            assert!(failure.contains(expected), "{failure}");
            assert!(!paths.device_pool_marker.exists());
        }
        fs::write(root.join("child-completed"), case).unwrap();
        return;
    }
    for case in ["same", "foreign", "ambiguous", "other-device"] {
        let root = fixture("existing-device-pool");
        fs::create_dir_all(root.path().join("sys/module/zfs")).unwrap();
        fs::create_dir(root.path().join("commands")).unwrap();
        fs::write(root.path().join("device"), "existing disk").unwrap();
        write_script(
            &root.path().join("commands/zpool"),
            r#"
case "$1" in
  list)
    if [ "$PLOYZ_EXISTING_DEVICE_POOL_CONTRACT" != foreign ]; then
      printf 'ployz-device\t8589934592\t0\t8589934592\tONLINE\toff\n'
    fi
    case "$PLOYZ_EXISTING_DEVICE_POOL_CONTRACT" in
      foreign|ambiguous) printf 'tank\t8589934592\t0\t8589934592\tONLINE\toff\n' ;;
    esac ;;
  status)
    backing="$PLOYZ_INSTALLER_CONTRACT_ROOT/device"
    [ "$PLOYZ_EXISTING_DEVICE_POOL_CONTRACT" != other-device ] || backing=/dev/another
    printf '  pool: ployz-device\nconfig:\n  NAME STATE\n  ployz-device ONLINE\n    %s ONLINE\n' "$backing" ;;
  *) echo unexpected host change >&2; exit 97 ;;
esac
"#,
        );
        run_contract_child_with_environment(
            "installer::storage::tests::named_storage_retries_only_the_same_unambiguous_device_pool",
            root.path(),
            CASE.into(),
            case.into(),
            case,
            [],
        );
    }
}

#[test]
fn smoke_pool_cleanup_preserves_backing_until_absence_is_proven() {
    const CASE: &str = "PLOYZ_SMOKE_CLEANUP_CASE";
    const ROOT: &str = "PLOYZ_SMOKE_CLEANUP_ROOT";
    if let Ok(case) = env::var(CASE) {
        let root = std::path::PathBuf::from(env::var_os(ROOT).unwrap());
        let stage = tempfile::tempdir_in(&root).unwrap();
        let directory = stage.path().to_owned();
        let backing = directory.join("backing");
        fs::write(&backing, b"pool data").unwrap();
        let result = validate_zfs_pool(stage);
        if case == "success" || case == "query-failed" {
            assert!(!directory.exists());
            assert_eq!(result.is_ok(), case == "success");
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains(&directory.display().to_string())
            );
            assert_eq!(fs::read(&backing).unwrap(), b"pool data", "{case}");
        }
        fs::write(root.join("completed"), case).unwrap();
        return;
    }

    for case in [
        "success",
        "query-failed",
        "destroy-failed",
        "pool-inspection-failed",
        "pool-remains",
        "dataset-inspection-failed",
        "dataset-remains",
        "create-unknown",
    ] {
        let root = tempfile::tempdir().unwrap();
        let commands = root.path().join("commands");
        fs::create_dir(&commands).unwrap();
        for (name, body) in [
            (
                "zpool",
                r#"
case "$1" in
    create)
        for arg do previous=${last-}; last=$arg; done
        echo "$previous" > "$PLOYZ_SMOKE_CLEANUP_ROOT/pool"
        [ "$PLOYZ_SMOKE_CLEANUP_CASE" != create-unknown ] ;;
    destroy) [ "$PLOYZ_SMOKE_CLEANUP_CASE" != destroy-failed ] ;;
    list)
        [ "$PLOYZ_SMOKE_CLEANUP_CASE" != create-unknown ] || exit 1
        if [ "$2" = -Hp ]; then
            [ "$PLOYZ_SMOKE_CLEANUP_CASE" != query-failed ]
        else
            [ "$PLOYZ_SMOKE_CLEANUP_CASE" != pool-inspection-failed ] || exit 1
            if [ "$PLOYZ_SMOKE_CLEANUP_CASE" = pool-remains ]; then
                /bin/cat "$PLOYZ_SMOKE_CLEANUP_ROOT/pool"
            fi
        fi ;;
esac
"#,
            ),
            (
                "zfs",
                r#"
if [ "$2" = -H ]; then
    [ "$PLOYZ_SMOKE_CLEANUP_CASE" != dataset-inspection-failed ] || exit 1
    if [ "$PLOYZ_SMOKE_CLEANUP_CASE" = dataset-remains ]; then
        /bin/cat "$PLOYZ_SMOKE_CLEANUP_ROOT/pool"
    fi
fi
"#,
            ),
        ] {
            let path = commands.join(name);
            fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(env::current_exe().unwrap())
            .args(["--exact", "installer::storage::tests::smoke_pool_cleanup_preserves_backing_until_absence_is_proven", "--nocapture"])
            .env(CASE, case)
            .env(ROOT, root.path())
            .env("PATH", commands)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(root.path().join("completed")).unwrap(),
            case
        );
    }
}

const ROOT: &str = "PLOYZ_INSTALLER_CONTRACT_ROOT";
const DEBIAN_13: &str = "NAME=\"Debian GNU/Linux\"\nVERSION_ID=\"13\"\nID=debian\n";
const AMAZON_LINUX_2023: &str = "NAME=\"Amazon Linux\"\nVERSION=\"2023\"\nID=\"amzn\"\nID_LIKE=\"fedora\"\nVERSION_ID=\"2023\"\n";

fn contract_root() -> PathBuf {
    PathBuf::from(env::var_os(ROOT).unwrap())
}

fn assert_refused(result: Result<(), Error>, expected: &str) {
    assert!(
        matches!(
            &result,
            Err(Error::Command { stage, message })
                if stage == "prepare ZFS storage" && message == expected
        ),
        "expected refusal {expected:?}, got {result:?}"
    );
}

/// Fakes that let `zfs_installed` see a module once `root/module` exists, on a VM whatever the
/// test runner itself is.
fn write_zfs_probes(commands: &Path) {
    write_script(&commands.join("systemd-detect-virt"), "echo kvm");
    write_script(
        &commands.join("modinfo"),
        r#"echo "$*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/modinfo.log"; [ -f "$PLOYZ_INSTALLER_CONTRACT_ROOT/module" ]"#,
    );
    write_script(&commands.join("zpool"), "exit 0");
    write_script(&commands.join("zfs"), "exit 0");
}

fn read_log(root: &Path, name: &str) -> String {
    fs::read_to_string(root.join(name)).unwrap_or_default()
}

struct RefusalCase {
    name: &'static str,
    os_release: &'static str,
    expected: &'static str,
}

const REFUSALS: [RefusalCase; 6] = [
    RefusalCase {
        name: "unsupported-distro",
        os_release: "NAME=\"Fedora Linux\"\nID=fedora\nVERSION_ID=42\n",
        expected: "Managed volumes need Ubuntu LTS, Debian 12–13 or Amazon Linux 2023; this Server runs Fedora Linux 42. Use one of those, or add `--storage none`.",
    },
    RefusalCase {
        name: "unsupported-version",
        os_release: "PRETTY_NAME=\"Debian GNU/Linux 11 (bullseye)\"\nNAME=\"Debian GNU/Linux\"\nVERSION_ID=\"11\"\nID=debian\n",
        expected: "Managed volumes need Ubuntu LTS, Debian 12–13 or Amazon Linux 2023; this Server runs Debian 11. Use one of those, or add `--storage none`.",
    },
    RefusalCase {
        name: "no-version",
        os_release: "NAME=\"Arch Linux\"\nID=arch\n",
        expected: "Managed volumes need Ubuntu LTS, Debian 12–13 or Amazon Linux 2023; this Server runs Arch Linux. Use one of those, or add `--storage none`.",
    },
    RefusalCase {
        name: "amazon-linux-2",
        os_release: "NAME=\"Amazon Linux\"\nVERSION=\"2\"\nID=\"amzn\"\nID_LIKE=\"centos rhel fedora\"\nVERSION_ID=\"2\"\n",
        expected: "Managed volumes need Ubuntu LTS, Debian 12–13 or Amazon Linux 2023; this Server runs Amazon Linux 2. Use one of those, or add `--storage none`.",
    },
    RefusalCase {
        name: "debian-secure-boot",
        os_release: DEBIAN_13,
        expected: "Secure Boot is on, so this Server can't load the ZFS module Ployz builds for Debian. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
    },
    RefusalCase {
        name: "amazon-secure-boot",
        os_release: AMAZON_LINUX_2023,
        expected: "Secure Boot is on, so this Server can't load the ZFS module Ployz builds for Amazon Linux. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
    },
];

#[test]
fn zfs_unsupported_os_contract() {
    const CASE: &str = "PLOYZ_ZFS_UNSUPPORTED_OS_CONTRACT";
    if let Ok(name) = env::var(CASE) {
        let case = REFUSALS.iter().find(|case| case.name == name).unwrap();
        let root = contract_root();
        let paths = InstallPaths::at(&root);
        assert_refused(
            prepare_storage(&StoragePreparation::Zfs { device: None }, &paths),
            case.expected,
        );
        assert!(!paths.modprobe_dir.exists());
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    for case in &REFUSALS {
        let fixture = fixture(case.name);
        let commands = fixture.path().join("commands");
        fs::create_dir_all(&commands).unwrap();
        fs::write(fixture.path().join("os-release"), case.os_release).unwrap();
        fs::create_dir_all(fixture.path().join("efivars")).unwrap();
        fs::write(fixture.path().join("efivars/SecureBoot"), [6, 0, 0, 0, 1]).unwrap();
        for command in [
            "apt-get",
            "apt-cache",
            "dpkg-query",
            "dpkg-deb",
            "dnf",
            "curl",
            "systemd-detect-virt",
            "uname",
            "modprobe",
            "fallocate",
            "zpool",
            "zfs",
        ] {
            write_script(
                &commands.join(command),
                "echo \"$0\" >> \"$PLOYZ_INSTALLER_FORBIDDEN\"; exit 97",
            );
        }
        let forbidden = fixture.path().join("forbidden-invocation");
        run_contract_child_with_environment(
            "installer::storage::tests::zfs_unsupported_os_contract",
            fixture.path(),
            OsString::from(CASE),
            OsString::from(case.name),
            case.name,
            [(
                OsString::from("PLOYZ_INSTALLER_FORBIDDEN"),
                forbidden.clone().into_os_string(),
            )],
        );
        assert!(!forbidden.exists(), "{} ran a host command", case.name);
    }
}

struct DebianCase {
    name: &'static str,
    version: &'static str,
    codename: &'static str,
    keyring: &'static str,
    kernel: &'static str,
    flavour: &'static str,
    /// The step the refusal names, when the build fails.
    failed_step: Option<&'static str>,
}

const DEBIAN_CASES: [DebianCase; 5] = [
    DebianCase {
        name: "debian-12",
        version: "12",
        codename: "bookworm",
        keyring: "debian-archive-keyring.gpg",
        kernel: "6.1.0-28-amd64",
        flavour: "amd64",
        failed_step: None,
    },
    DebianCase {
        name: "debian-13-cloud",
        version: "13",
        codename: "trixie",
        keyring: "debian-archive-keyring.pgp",
        kernel: "6.12.43+deb13-cloud-amd64",
        flavour: "cloud-amd64",
        failed_step: None,
    },
    DebianCase {
        name: "build-failed",
        version: "13",
        codename: "trixie",
        keyring: "debian-archive-keyring.pgp",
        kernel: "6.12.43+deb13-cloud-amd64",
        flavour: "cloud-amd64",
        failed_step: Some("installing zfs-dkms"),
    },
    DebianCase {
        name: "no-module",
        version: "13",
        codename: "trixie",
        keyring: "debian-archive-keyring.pgp",
        kernel: "6.12.43+deb13-cloud-amd64",
        flavour: "cloud-amd64",
        failed_step: Some("the ZFS module check"),
    },
    // zfs-dkms is already installed, so apt-get builds nothing for the running kernel.
    DebianCase {
        name: "packaged-unbuilt",
        version: "12",
        codename: "bookworm",
        keyring: "debian-archive-keyring.gpg",
        kernel: "6.1.0-28-amd64",
        flavour: "amd64",
        failed_step: None,
    },
];

#[test]
fn zfs_debian_contract() {
    const CASE: &str = "PLOYZ_ZFS_DEBIAN_CONTRACT";

    if let Ok(name) = env::var(CASE) {
        let case = DEBIAN_CASES.iter().find(|case| case.name == name).unwrap();
        let root = contract_root();
        let paths = InstallPaths::at(&root);
        let os = OsRelease::read(&paths.os_release).unwrap();
        let ZfsRoute::Debian { codename, keyring } = ZfsRoute::for_os(&os).unwrap() else {
            panic!("{name} is not on the Debian route");
        };
        let result = prepare_debian_zfs(&paths.apt_dir, &os, codename, keyring, case.kernel, None);
        match case.failed_step {
            Some(step) => assert_refused(
                result,
                &format!(
                    "Couldn't build ZFS for kernel {} on Debian {}: {step} failed. Add `--storage none` to start without managed volumes.",
                    case.kernel, case.version
                ),
            ),
            None => result.unwrap(),
        }
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    for case in &DEBIAN_CASES {
        let fixture = fixture(case.name);
        let root = fixture.path();
        let commands = root.join("commands");
        let sources = root.join("apt/sources.list.d");
        fs::create_dir_all(&commands).unwrap();
        fs::create_dir_all(&sources).unwrap();
        fs::write(
            root.join("os-release"),
            format!(
                "NAME=\"Debian GNU/Linux\"\nVERSION_ID=\"{}\"\nID=debian\n",
                case.version
            ),
        )
        .unwrap();
        write_script(
            &commands.join("apt-get"),
            r#"echo "$DEBIAN_FRONTEND $*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/apt.log"
case "$*" in
  *zfs-dkms*)
    if [ "$PLOYZ_ZFS_DEBIAN_CONTRACT" = build-failed ]; then echo "dkms build failed" >&2; exit 100; fi
    case "$PLOYZ_ZFS_DEBIAN_CONTRACT" in no-module|packaged-unbuilt) ;; *) : > "$PLOYZ_INSTALLER_CONTRACT_ROOT/module" ;; esac ;;
esac"#,
        );
        // zfs-dkms lists its source even when its install already built the module, which
        // leaves nothing for `dkms install` to do.
        write_script(
            &commands.join("dpkg"),
            r#"[ "$PLOYZ_ZFS_DEBIAN_CONTRACT" != no-module ] && [ "$*" = '-L zfs-dkms' ] && printf '/.\n/usr/src\n/usr/src/zfs-2.1.11\n/usr/src/zfs-2.1.11/dkms.conf\n'"#,
        );
        write_script(
            &commands.join("dkms"),
            r#"echo "$*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/dkms.log"
[ "$1" = install ] && : > "$PLOYZ_INSTALLER_CONTRACT_ROOT/module""#,
        );
        write_zfs_probes(&commands);
        run_contract_child_with_environment(
            "installer::storage::tests::zfs_debian_contract",
            root,
            OsString::from(CASE),
            OsString::from(case.name),
            case.name,
            [],
        );

        let name = case.name;
        let contrib = fs::read_to_string(sources.join("ployz-contrib.sources")).unwrap();
        assert!(
            contrib.contains(&format!("Suites: {0} {0}-updates\n", case.codename))
                && contrib.contains(&format!("Suites: {}-security\n", case.codename))
                && contrib.contains(&format!(
                    "Signed-By: /usr/share/keyrings/{}\n",
                    case.keyring
                ))
                && contrib.contains("Components: contrib\n"),
            "{name}: {contrib}"
        );
        let apt = read_log(root, "apt.log");
        let calls: Vec<Vec<&str>> = apt.lines().map(|line| line.split(' ').collect()).collect();
        assert!(
            calls
                .iter()
                .all(|words| words.first() == Some(&"noninteractive")),
            "{name}: {apt}"
        );
        let installs = |packages: &[&str]| {
            calls
                .iter()
                .position(|words| packages.iter().all(|package| words.contains(package)))
                .unwrap_or_else(|| panic!("{name}: no install of {packages:?} in\n{apt}"))
        };
        let headers = installs(&[
            &format!("linux-headers-{}", case.kernel),
            &format!("linux-headers-{}", case.flavour),
        ]);
        let zfs = installs(&["zfs-dkms", "zfsutils-linux"]);
        assert!(headers < zfs, "{name}: headers after zfs-dkms\n{apt}");
        let dkms = read_log(root, "dkms.log");
        if name == "packaged-unbuilt" {
            assert_eq!(
                dkms,
                format!("install -m zfs -v 2.1.11 -k {}\n", case.kernel),
                "{name}"
            );
        } else {
            assert_eq!(dkms, "", "{name}");
        }
        assert!(
            read_log(root, "modinfo.log")
                .lines()
                .all(|line| line == format!("-k {} zfs", case.kernel)),
            "{name}"
        );
    }
}

#[test]
fn zfs_install_takes_over_a_prepared_build_contract() {
    use crate::installer::prepare::{PrepState, ZfsBuild, discard_at};

    const CASE: &str = "PLOYZ_ZFS_TAKEOVER_CONTRACT";
    const KERNEL: &str = "6.12.43+deb13-cloud-amd64";
    if let Ok(name) = env::var(CASE) {
        let root = contract_root();
        let paths = InstallPaths::at(&root);
        let os = OsRelease::read(&paths.os_release).unwrap();
        ZfsRoute::for_os(&os)
            .unwrap()
            .install(&paths, &os, KERNEL)
            .unwrap();
        if name == "no-build" {
            discard_at(&paths).unwrap();
        }
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    for name in [
        "owned",
        "mismatch",
        "no-build",
        "stuck-cleanup",
        "contended",
        "unprepared",
        "built",
        "built-unknown",
        "unknown",
    ] {
        let fixture = fixture(name);
        let root = fixture.path();
        let commands = root.join("commands");
        fs::create_dir_all(&commands).unwrap();
        fs::create_dir_all(root.join("apt/sources.list.d")).unwrap();
        fs::write(root.join("os-release"), DEBIAN_13).unwrap();
        write_script(
            &commands.join("apt-get"),
            r#"lock="$PLOYZ_INSTALLER_CONTRACT_ROOT/data/prepare/lock"
if [ -e "$lock" ] && ! flock -n "$lock" true; then held=locked; else held=unlocked; fi
echo "$held $*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/apt.log"
case "$*" in
  *zfs-dkms*)
    : > "$PLOYZ_INSTALLER_CONTRACT_ROOT/module"
    : > "$PLOYZ_INSTALLER_CONTRACT_ROOT/zfs-dkms" ;;
esac"#,
        );
        write_script(
            &commands.join("dpkg-query"),
            r#"[ "$3" = zfs-dkms ] && [ -f "$PLOYZ_INSTALLER_CONTRACT_ROOT/zfs-dkms" ] && printf 'install ok installed'"#,
        );
        write_script(
            &commands.join("dpkg"),
            r#"[ "$1" = -S ] && [ "$2" = "$PLOYZ_INSTALLER_CONTRACT_ROOT/usr/src/zfs-2.3.2" ] || exit 2
case "$PLOYZ_ZFS_TAKEOVER_CONTRACT" in *unknown) echo 'dpkg-query: error: unexpected end of file in diversions' >&2; exit 2 ;; esac
[ -f "$PLOYZ_INSTALLER_CONTRACT_ROOT/zfs-dkms" ] && [ "$PLOYZ_ZFS_TAKEOVER_CONTRACT" != mismatch ]"#,
        );
        write_script(
            &commands.join("dkms"),
            r#"echo "$*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/dkms.log"
case "$1" in
  status)
    if [ "${PLOYZ_ZFS_TAKEOVER_CONTRACT#built}" = "$PLOYZ_ZFS_TAKEOVER_CONTRACT" ]; then echo "zfs/2.3.2: added"
    elif [ -f "$PLOYZ_INSTALLER_CONTRACT_ROOT/module" ]; then echo "zfs/2.3.2, 6.12.43+deb13-cloud-amd64, x86_64: installed"
    else echo "zfs/2.3.2, 6.12.43+deb13-cloud-amd64, x86_64: built"; fi ;;
  install) : > "$PLOYZ_INSTALLER_CONTRACT_ROOT/module" ;;
esac"#,
        );
        symlink("/usr/bin/flock", commands.join("flock")).unwrap();
        write_zfs_probes(&commands);
        let prepare = root.join("data/prepare");
        let source = root.join("usr/src/zfs-2.3.2");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("dkms.conf"),
            "PACKAGE_NAME=\"zfs\"\nAUTOINSTALL=\"no\"\n",
        )
        .unwrap();
        let claim = PrepState {
            added_packages: vec!["dkms".into(), "lsb-release".into()],
            zfs_build: (name != "no-build").then(|| ZfsBuild::Dkms {
                module: "zfs".into(),
                version: "2.3.2".into(),
                source_dir: source.clone(),
            }),
            added_contrib_source: true,
            ..PrepState::default()
        };
        if name != "unprepared" {
            fs::create_dir_all(&prepare).unwrap();
            if name == "stuck-cleanup" {
                fs::write(prepare.join("zfs"), b"not a directory").unwrap();
            } else {
                fs::create_dir_all(prepare.join("zfs/build")).unwrap();
            }
            fs::write(
                prepare.join("state.json"),
                serde_json::to_vec(&claim).unwrap(),
            )
            .unwrap();
        }
        let holder = (name == "contended").then(|| {
            let held = root.join("held");
            let holder = Command::new("/usr/bin/flock")
                .arg(prepare.join("lock"))
                .args(["sh", "-c", "touch \"$0\"; sleep 1"])
                .arg(&held)
                .spawn()
                .unwrap();
            while !held.exists() {
                // real-wait: the flock child signals that it holds the lock only by touching a file.
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            holder
        });
        let output = run_contract_child_with_environment(
            "installer::storage::tests::zfs_install_takes_over_a_prepared_build_contract",
            root,
            OsString::from(CASE),
            OsString::from(name),
            name,
            [],
        );
        if let Some(mut holder) = holder {
            holder.wait().unwrap();
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.contains("Waiting for host preparation to finish"),
            name == "contended",
            "{name}: {stdout}"
        );
        let apt = read_log(root, "apt.log");
        let held = if name == "unprepared" {
            "unlocked"
        } else {
            "locked"
        };
        assert!(
            !apt.is_empty() && apt.lines().all(|line| line.starts_with(held)),
            "{name}: {apt}"
        );
        assert!(
            !apt.lines().any(|line| line.contains(" remove ")),
            "{name}: {apt}"
        );
        if name == "unprepared" {
            assert!(!prepare.exists(), "{name}");
            continue;
        }
        let state: PrepState =
            serde_json::from_slice(&fs::read(prepare.join("state.json")).unwrap()).unwrap();
        // An installed build is released whoever owns it; an unknown owner keeps any other.
        if name == "unknown" {
            assert_eq!(state.zfs_build, claim.zfs_build, "{name}");
        } else {
            assert_eq!(state.zfs_build, None, "{name}");
        }
        assert!(state.added_packages.is_empty(), "{name}");
        assert!(!state.added_contrib_source, "{name}");
        let dkms = read_log(root, "dkms.log");
        if name == "mismatch" {
            assert!(
                dkms.contains("remove -m zfs -v 2.3.2 --all"),
                "{name}: {dkms}"
            );
            assert!(!source.exists(), "{name}");
        } else {
            assert!(!dkms.contains("remove"), "{name}: {dkms}");
            assert!(source.exists(), "{name}");
        }
        let conf = fs::read_to_string(source.join("dkms.conf")).unwrap_or_default();
        if name.starts_with("built") {
            assert!(
                dkms.contains(&format!("install -m zfs -v 2.3.2 -k {KERNEL}")),
                "{name}: {dkms}"
            );
            assert!(
                !apt.contains("zfs-dkms")
                    && apt.contains("install -y -qq --no-install-recommends zfsutils-linux"),
                "{name}: {apt}"
            );
            assert!(conf.contains("AUTOINSTALL=\"yes\""), "{name}: {conf}");
        } else {
            assert!(!dkms.contains("install"), "{name}: {dkms}");
            assert!(apt.contains("zfs-dkms"), "{name}: {apt}");
        }
        if name == "stuck-cleanup" {
            assert!(prepare.join("zfs").is_file(), "{name}");
        } else {
            assert!(!prepare.join("zfs").exists(), "{name}");
        }
    }
}

const AMAZON_KERNEL: &str = "6.1.186-228.376.amzn2023.x86_64";

struct AmazonCase {
    name: &'static str,
    /// Runs the whole of `prepare_storage` rather than only the build route.
    via_storage: bool,
    expected: Option<&'static str>,
}

const AMAZON_CASES: [AmazonCase; 5] = [
    AmazonCase {
        name: "success",
        via_storage: false,
        expected: None,
    },
    // zfs-dkms is already installed, so dnf builds nothing for the running kernel.
    AmazonCase {
        name: "packaged-unbuilt",
        via_storage: false,
        expected: None,
    },
    AmazonCase {
        name: "prepared",
        via_storage: false,
        expected: None,
    },
    AmazonCase {
        name: "checksum-mismatch",
        via_storage: false,
        expected: Some(
            "Couldn't build ZFS for kernel 6.1.186-228.376.amzn2023.x86_64 on Amazon Linux 2023: the OpenZFS checksum check failed. Add `--storage none` to start without managed volumes.",
        ),
    },
    AmazonCase {
        name: "no-kernel-devel",
        via_storage: true,
        expected: Some(
            "Amazon Linux 2023 has no kernel-devel package for the running kernel 6.1.186-228.376.amzn2023.x86_64, so ZFS can't be built for it. Update the kernel, reboot, and retry, or add `--storage none`.",
        ),
    },
];

#[test]
fn zfs_amazon_linux_contract() {
    use sha2::{Digest, Sha256};

    const CASE: &str = "PLOYZ_ZFS_AMAZON_CONTRACT";
    if let Ok(name) = env::var(CASE) {
        let case = AMAZON_CASES.iter().find(|case| case.name == name).unwrap();
        let root = contract_root();
        let paths = InstallPaths::at(&root);
        let result = if case.via_storage {
            prepare_storage(&StoragePreparation::Zfs { device: None }, &paths)
        } else {
            let os = OsRelease::read(&paths.os_release).unwrap();
            let tarball = if name == "checksum-mismatch" {
                b"a different tarball".to_vec()
            } else {
                fs::read(root.join("tarball")).unwrap()
            };
            prepare_amazon_zfs(
                &root.join("prepared"),
                &paths.modules_load_dir,
                &os,
                AMAZON_KERNEL,
                &hex::encode(Sha256::digest(tarball)),
            )
        };
        match case.expected {
            Some(expected) => assert_refused(result, expected),
            None => result.unwrap(),
        }
        assert!(!paths.modprobe_dir.exists());
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    for case in &AMAZON_CASES {
        let fixture = fixture(case.name);
        let root = fixture.path();
        let commands = root.join("commands");
        fs::create_dir_all(&commands).unwrap();
        fs::write(root.join("os-release"), AMAZON_LINUX_2023).unwrap();
        let release = root.join("release/zfs-2.4.4");
        fs::create_dir_all(&release).unwrap();
        write_script(&release.join("configure"), "exit 0");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(root.join("tarball"))
            .arg("-C")
            .arg(root.join("release"))
            .arg("zfs-2.4.4")
            .status()
            .unwrap();
        assert!(status.success());
        write_script(
            &commands.join("dnf"),
            r#"echo "$*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/dnf.log"
case "$*" in
  *repoquery*) [ "$PLOYZ_ZFS_AMAZON_CONTRACT" = no-kernel-devel ] || echo kernel-devel-1:6.1.186-228.376.amzn2023.x86_64 ;;
  *zfs-dkms-*.rpm*) [ "$PLOYZ_ZFS_AMAZON_CONTRACT" = packaged-unbuilt ] || : > "$PLOYZ_INSTALLER_CONTRACT_ROOT/module" ;;
esac"#,
        );
        write_script(
            &commands.join("rpm"),
            r#"[ "$*" = '-ql zfs-dkms' ] && printf '/usr/src/zfs-2.4.4\n/usr/src/zfs-2.4.4/dkms.conf\n'"#,
        );
        write_script(
            &commands.join("dkms"),
            r#"echo "$*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/dkms.log"
[ "$1" = install ] && : > "$PLOYZ_INSTALLER_CONTRACT_ROOT/module""#,
        );
        write_script(
            &commands.join("curl"),
            r#"echo "$*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/curl.log"
while [ "$1" != -o ]; do shift; done
cat < "$PLOYZ_INSTALLER_CONTRACT_ROOT/tarball" > "$2""#,
        );
        // make leaves every package the real rpmbuild does, including ones not to install.
        write_script(
            &commands.join("make"),
            r#"for package in zfs-2.4.4-1.amzn2023.src zfs-dkms-2.4.4-1.amzn2023.src zfs-dkms-2.4.4-1.amzn2023.noarch zfs-2.4.4-1.amzn2023.x86_64 zfs-debuginfo-2.4.4-1.amzn2023.x86_64 zfs-test-2.4.4-1.amzn2023.x86_64 zfs-dracut-2.4.4-1.amzn2023.noarch libzfs7-2.4.4-1.amzn2023.x86_64 libzfs7-devel-2.4.4-1.amzn2023.x86_64 libzpool7-2.4.4-1.amzn2023.x86_64 libnvpair3-2.4.4-1.amzn2023.x86_64 libuutil3-2.4.4-1.amzn2023.x86_64 python3-pyzfs-2.4.4-1.amzn2023.noarch; do
  : > "$package.rpm"
done"#,
        );
        write_script(&commands.join("uname"), &format!("echo {AMAZON_KERNEL}"));
        for tool in ["tar", "gzip", "cat"] {
            symlink(format!("/usr/bin/{tool}"), commands.join(tool)).unwrap();
        }
        write_zfs_probes(&commands);
        if case.name == "prepared" {
            let built = root.join("prepared/zfs-2.4.4");
            fs::create_dir_all(&built).unwrap();
            Command::new(commands.join("make"))
                .current_dir(&built)
                .status()
                .unwrap();
        }
        run_contract_child_with_environment(
            "installer::storage::tests::zfs_amazon_linux_contract",
            root,
            OsString::from(CASE),
            OsString::from(case.name),
            case.name,
            [],
        );

        let name = case.name;
        let boot_load = fs::read_to_string(root.join("modules-load/ployz-zfs.conf")).ok();
        let dnf = read_log(root, "dnf.log");
        let installs: Vec<&str> = dnf
            .lines()
            .filter(|line| line.starts_with("install "))
            .collect();
        if case.expected.is_some() {
            assert!(installs.is_empty(), "{name} installed packages:\n{dnf}");
            assert_eq!(boot_load, None, "{name}");
            continue;
        }
        assert_eq!(boot_load.as_deref(), Some("zfs\n"));
        let dkms = read_log(root, "dkms.log");
        if name == "packaged-unbuilt" {
            assert_eq!(
                dkms,
                format!("install -m zfs -v 2.4.4 -k {AMAZON_KERNEL}\n")
            );
        } else {
            assert_eq!(dkms, "", "{name}");
        }
        let packages = if name == "prepared" {
            assert_eq!(
                read_log(root, "curl.log"),
                "",
                "the prepared build is not downloaded"
            );
            let [packages] = installs.as_slice() else {
                panic!("the prepared build is installed without its build dependencies:\n{dnf}");
            };
            assert!(
                packages.contains(&root.join("prepared/zfs-2.4.4").display().to_string()),
                "{packages}"
            );
            packages
        } else {
            let [dependencies, packages] = installs.as_slice() else {
                panic!("unexpected dnf installs:\n{dnf}");
            };
            assert!(
                dependencies.contains(&format!("kernel-devel-uname-r = {AMAZON_KERNEL}")),
                "{dependencies}"
            );
            packages
        };
        let mut rpms: Vec<&str> = packages
            .split(' ')
            .filter_map(|word| word.rsplit('/').next())
            .filter(|word| word.ends_with(".rpm"))
            .collect();
        rpms.sort_unstable();
        assert_eq!(
            rpms,
            [
                "libnvpair3-2.4.4-1.amzn2023.x86_64.rpm",
                "libuutil3-2.4.4-1.amzn2023.x86_64.rpm",
                "libzfs7-2.4.4-1.amzn2023.x86_64.rpm",
                "libzpool7-2.4.4-1.amzn2023.x86_64.rpm",
                "zfs-2.4.4-1.amzn2023.x86_64.rpm",
                "zfs-dkms-2.4.4-1.amzn2023.noarch.rpm",
            ]
        );
    }
}

#[test]
fn zfs_already_installed_contract() {
    const CASE: &str = "PLOYZ_ZFS_INSTALLED_CONTRACT";
    if let Ok(name) = env::var(CASE) {
        let root = contract_root();
        // Past the build, this ends at the host-root reserve or the fake modprobe; either way
        // the parent checks nothing was installed.
        assert!(
            prepare_storage(
                &StoragePreparation::Zfs { device: None },
                &InstallPaths::at(&root)
            )
            .is_err()
        );
        fs::write(root.join("child-completed"), name).unwrap();
        return;
    }

    for (name, os_release, kernel) in [
        ("debian", DEBIAN_13, "6.12.43+deb13-cloud-amd64"),
        ("amazon-linux", AMAZON_LINUX_2023, AMAZON_KERNEL),
    ] {
        let fixture = fixture(name);
        let root = fixture.path();
        let commands = root.join("commands");
        fs::create_dir_all(&commands).unwrap();
        fs::write(root.join("os-release"), os_release).unwrap();
        fs::write(root.join("module"), "").unwrap();
        for command in ["apt-get", "dnf", "curl"] {
            write_script(
                &commands.join(command),
                "echo \"$0 $*\" >> \"$PLOYZ_INSTALLER_CONTRACT_ROOT/forbidden\"; exit 97",
            );
        }
        write_script(&commands.join("uname"), &format!("echo {kernel}"));
        write_script(&commands.join("modprobe"), "exit 1");
        write_zfs_probes(&commands);
        run_contract_child_with_environment(
            "installer::storage::tests::zfs_already_installed_contract",
            root,
            OsString::from(CASE),
            OsString::from(name),
            name,
            [],
        );
        assert_eq!(read_log(root, "forbidden"), "", "{name}");
        assert_eq!(
            read_log(root, "modinfo.log"),
            format!("-k {kernel} zfs\n"),
            "{name}"
        );
    }
}

#[test]
fn debian_kernel_flavour_names_header_packages() {
    assert_eq!(debian_kernel_flavour("6.1.0-28-amd64"), Some("amd64"));
    assert_eq!(
        debian_kernel_flavour("6.12.43+deb13-cloud-arm64"),
        Some("cloud-arm64")
    );
}

#[test]
fn device_creation_commits_ownership_only_after_confirmation() {
    const CASE: &str = "PLOYZ_DEVICE_CREATE_CONTRACT";
    if let Ok(case) = env::var(CASE) {
        let root = contract_root();
        let paths = InstallPaths::at(&root);
        let device = root.join("device");
        let assessed = if case == "path-changed" {
            root.join("old-device")
        } else {
            device.clone()
        };
        let result = create_device_pool(&device, &assessed, &paths);
        if case == "success" {
            result.unwrap();
            assert!(paths.device_pool_marker.exists());
        } else {
            assert!(result.is_err());
            assert!(!paths.device_pool_marker.exists());
        }
        fs::write(root.join("child-completed"), case).unwrap();
        return;
    }
    for case in ["create-failed", "path-changed", "not-imported", "success"] {
        let root = fixture(case);
        fs::create_dir(root.path().join("commands")).unwrap();
        fs::write(root.path().join("device"), "selected disk").unwrap();
        write_script(
            &root.path().join("commands/zpool"),
            r#"
case "$1" in
 create) [ "$PLOYZ_DEVICE_CREATE_CONTRACT" != create-failed ] || exit 1 ;;
 list) [ "$PLOYZ_DEVICE_CREATE_CONTRACT" != success ] || printf 'ployz-device\t8589934592\t0\t8589934592\tONLINE\toff\n' ;;
 import) : ;;
 status) printf 'pool: ployz-device\nconfig:\n  %s/device ONLINE\n' "$PLOYZ_INSTALLER_CONTRACT_ROOT" ;;
 *) exit 97 ;;
esac
"#,
        );
        run_contract_child_with_environment(
            "installer::storage::tests::device_creation_commits_ownership_only_after_confirmation",
            root.path(),
            CASE.into(),
            case.into(),
            case,
            [],
        );
    }
}

#[test]
fn plain_zfs_reconciles_stale_ownership_only_with_proven_file_pool_evidence() {
    const CASE: &str = "PLOYZ_MARKER_RECONCILE_CONTRACT";
    if let Ok(case) = env::var(CASE) {
        let root = contract_root();
        let paths = InstallPaths::at(&root);
        let result = reconcile_device_pool_marker(&paths);
        match case.as_str() {
            "retained-file" => {
                result.unwrap();
                assert!(!paths.device_pool_marker.exists());
            }
            "exported-device" | "imported-device" => {
                result.unwrap();
                assert!(paths.device_pool_marker.exists());
            }
            _ => {
                assert!(result.is_err());
                assert!(paths.device_pool_marker.exists());
            }
        }
        fs::write(root.join("child-completed"), case).unwrap();
        return;
    }
    for case in [
        "retained-file",
        "owned-missing",
        "owned-with-file",
        "unlabeled-file",
        "exported-device",
        "imported-device",
    ] {
        let root = fixture(case);
        fs::create_dir(root.path().join("commands")).unwrap();
        fs::write(root.path().join("machine-pool.device"), "ployz-device\n").unwrap();
        if case == "owned-with-file" {
            fs::write(
                root.path().join("machine-pool.device"),
                crate::machine_pool::CONFIRMED_DEVICE_POOL_MARKER,
            )
            .unwrap();
        }
        if case.ends_with("file") {
            fs::write(root.path().join("machine-pool"), "existing backing bytes").unwrap();
        }
        write_script(
            &root.path().join("commands/zpool"),
            r#"
case "$1" in
 list) [ "$PLOYZ_MARKER_RECONCILE_CONTRACT" != imported-device ] || echo ployz-device ;;
 import)
   if [ "$#" = 1 ]; then
     [ "$PLOYZ_MARKER_RECONCILE_CONTRACT" != exported-device ] || printf 'pool: ployz-device\n'
   elif [ "$PLOYZ_MARKER_RECONCILE_CONTRACT" = retained-file ] || [ "$PLOYZ_MARKER_RECONCILE_CONTRACT" = owned-with-file ]; then printf 'pool: ployz\n'
   fi ;;
 *) exit 97 ;;
esac
"#,
        );
        run_contract_child_with_environment(
            "installer::storage::tests::plain_zfs_reconciles_stale_ownership_only_with_proven_file_pool_evidence",
            root.path(),
            CASE.into(),
            case.into(),
            case,
            [],
        );
    }
}

#[test]
fn named_storage_retry_imports_only_exported_pool_on_the_requested_disk() {
    const CASE: &str = "PLOYZ_DEVICE_IMPORT_CONTRACT";
    if let Ok(case) = env::var(CASE) {
        let root = contract_root();
        let paths = InstallPaths::at(&root);
        let storage =
            StoragePreparation::parse(format!("zfs:{}", root.join("device").display())).unwrap();
        let result = prepare_storage(&storage, &paths);
        if matches!(case.as_str(), "exported" | "unloaded") {
            result.unwrap();
            assert!(paths.device_pool_marker.exists());
            assert!(root.join("imported").exists());
        } else {
            assert!(result.is_err());
            assert!(!paths.device_pool_marker.exists());
            assert!(!root.join("imported").exists());
        }
        fs::write(root.join("child-completed"), case).unwrap();
        return;
    }
    for case in ["exported", "unloaded", "other-device", "file-backing"] {
        let root = fixture(case);
        fs::create_dir(root.path().join("commands")).unwrap();
        if case != "unloaded" {
            fs::create_dir_all(root.path().join("sys/module/zfs")).unwrap();
        }
        if case == "file-backing" {
            fs::write(root.path().join("machine-pool"), "retained file Pool").unwrap();
        }
        fs::write(root.path().join("device"), "labelled disk").unwrap();
        write_script(&root.path().join("commands/modprobe"), "exit 0");
        write_script(
            &root.path().join("commands/zpool"),
            r#"
case "$1" in
 list) [ ! -e "$PLOYZ_INSTALLER_CONTRACT_ROOT/imported" ] || printf 'ployz-device\t8589934592\t0\t8589934592\tONLINE\toff\n' ;;
 import)
   if [ "$#" = 1 ]; then
     backing="$PLOYZ_INSTALLER_CONTRACT_ROOT/device"
     [ "$PLOYZ_DEVICE_IMPORT_CONTRACT" != other-device ] || backing=/dev/other
     printf 'pool: ployz-device\nconfig:\n  %s ONLINE\n' "$backing"
   else : > "$PLOYZ_INSTALLER_CONTRACT_ROOT/imported"; fi ;;
 status) printf 'pool: ployz-device\nconfig:\n  %s/device ONLINE\n' "$PLOYZ_INSTALLER_CONTRACT_ROOT" ;;
 *) exit 97 ;;
esac
"#,
        );
        run_contract_child_with_environment(
            "installer::storage::tests::named_storage_retry_imports_only_exported_pool_on_the_requested_disk",
            root.path(),
            CASE.into(),
            case.into(),
            case,
            [],
        );
    }
}
