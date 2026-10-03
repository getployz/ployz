//! ZFS preparation contracts with isolated host command adapters.

use super::*;
use crate::installer::test_support::{fixture, run_contract_child_with_environment, write_script};
use std::{
    env,
    ffi::OsString,
    os::unix::fs::{PermissionsExt, symlink},
};

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

/// Fakes that let `zfs_installed` see a module once `root/module` exists.
fn write_zfs_probes(commands: &Path) {
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
        assert_refused(prepare_storage(StorageChoice::Zfs, &paths), case.expected);
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

const DEBIAN_CASES: [DebianCase; 4] = [
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
        let result = prepare_debian_zfs(&paths.apt_dir, &os, codename, keyring, case.kernel);
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
    if [ "$PLOYZ_ZFS_DEBIAN_CONTRACT" != no-module ]; then : > "$PLOYZ_INSTALLER_CONTRACT_ROOT/module"; fi ;;
esac"#,
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
        assert!(
            read_log(root, "modinfo.log")
                .lines()
                .all(|line| line == format!("-k {} zfs", case.kernel)),
            "{name}"
        );
    }
}

const AMAZON_KERNEL: &str = "6.1.186-228.376.amzn2023.x86_64";

struct AmazonCase {
    name: &'static str,
    /// Runs the whole of `prepare_storage` rather than only the build route.
    via_storage: bool,
    expected: Option<&'static str>,
}

const AMAZON_CASES: [AmazonCase; 3] = [
    AmazonCase {
        name: "success",
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
            prepare_storage(StorageChoice::Zfs, &paths)
        } else {
            let os = OsRelease::read(&paths.os_release).unwrap();
            let tarball = if name == "checksum-mismatch" {
                b"a different tarball".to_vec()
            } else {
                fs::read(root.join("tarball")).unwrap()
            };
            prepare_amazon_zfs(
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
  *zfs-dkms-*.rpm*) : > "$PLOYZ_INSTALLER_CONTRACT_ROOT/module" ;;
esac"#,
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
        if name != "success" {
            assert!(installs.is_empty(), "{name} installed packages:\n{dnf}");
            assert_eq!(boot_load, None, "{name}");
            continue;
        }
        assert_eq!(boot_load.as_deref(), Some("zfs\n"));
        let [dependencies, packages] = installs.as_slice() else {
            panic!("unexpected dnf installs:\n{dnf}");
        };
        assert!(
            dependencies.contains(&format!("kernel-devel-uname-r = {AMAZON_KERNEL}")),
            "{dependencies}"
        );
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
        assert!(prepare_storage(StorageChoice::Zfs, &InstallPaths::at(&root)).is_err());
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
fn secure_boot_reading_follows_test_root() {
    let fixture = fixture("secure-boot");
    let paths = InstallPaths::at(fixture.path());
    assert!(!secure_boot_enabled(&paths).unwrap());
    fs::create_dir_all(paths.secure_boot.parent().unwrap()).unwrap();
    for (variable, enabled) in [([6, 0, 0, 0, 1], true), ([6, 0, 0, 0, 0], false)] {
        fs::write(&paths.secure_boot, variable).unwrap();
        assert_eq!(secure_boot_enabled(&paths).unwrap(), enabled);
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
