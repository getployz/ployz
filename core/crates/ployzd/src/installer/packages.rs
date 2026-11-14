//! The Machine's package manager and the questions installer steps ask it, so preparation,
//! discard and install read the same answers the same way.

use std::{ffi::OsStr, path::Path, process::Command};

use ployz_core::host_preparation::ZfsBuild;
use ployz_core::probe_document::OsRelease;

use super::apt_get;
use super::assess::Fact;

/// The package manager of a supported distribution family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PackageManager {
    Apt,
    Dnf,
}

impl PackageManager {
    /// The manager of `os`'s family: Debian and Ubuntu use apt, Amazon Linux uses dnf.
    pub(super) fn of(os: &OsRelease) -> Option<Self> {
        match os.id.as_str() {
            "debian" | "ubuntu" => Some(Self::Apt),
            _ if os.is_amazon_linux() => Some(Self::Dnf),
            _ => None,
        }
    }

    pub(super) const fn command(self) -> &'static str {
        match self {
            Self::Apt => "apt-get",
            Self::Dnf => "dnf",
        }
    }

    /// Installs `packages`, with apt's recommendations left out.
    pub(super) fn install<S: AsRef<OsStr>>(self, packages: impl IntoIterator<Item = S>) -> Command {
        let mut command = match self {
            Self::Apt => {
                let mut apt = apt_get();
                apt.args(["install", "-y", "-qq", "--no-install-recommends"]);
                apt
            }
            Self::Dnf => {
                let mut dnf = Command::new("dnf");
                dnf.args(["install", "-y"]);
                dnf
            }
        };
        command.args(packages);
        command
    }

    pub(super) fn remove<S: AsRef<OsStr>>(self, packages: impl IntoIterator<Item = S>) -> Command {
        let mut command = match self {
            Self::Apt => {
                let mut apt = apt_get();
                apt.args(["remove", "-y", "-qq"]);
                apt
            }
            Self::Dnf => {
                let mut dnf = Command::new("dnf");
                dnf.args(["remove", "-y"]);
                dnf
            }
        };
        command.args(packages);
        command
    }

    /// The packages of `wanted` the Machine doesn't have. A package the manager can't be asked
    /// about is reported and counted as missing.
    pub(super) fn lacking(self, wanted: impl IntoIterator<Item = String>) -> Vec<String> {
        wanted
            .into_iter()
            .filter(|package| {
                let (asked, answer) = match self {
                    Self::Apt => ("dpkg", deb_installed(package)),
                    Self::Dnf => ("rpm", rpm_provided(package)),
                };
                !answer.or_report(
                    &format!("ask {asked} whether {package} is installed"),
                    false,
                )
            })
            .collect()
    }

    /// The command that lists the files of an installed package.
    const fn list_files(self) -> [&'static str; 2] {
        match self {
            Self::Apt => ["dpkg", "-L"],
            Self::Dnf => ["rpm", "-ql"],
        }
    }

    /// The DKMS version of the installed `zfs-dkms`, read from the source directory it ships.
    pub(super) fn zfs_dkms_version(self) -> Option<String> {
        let [command, flag] = self.list_files();
        let output = Command::new(command)
            .args([flag, "zfs-dkms"])
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| {
                line.strip_prefix("/usr/src/zfs-")
                    .filter(|version| !version.is_empty() && !version.contains('/'))
                    .map(str::to_owned)
            })
    }

    /// Whether `zfs-dkms` is installed, or why the package manager couldn't say.
    pub(super) fn zfs_dkms_installed(self) -> Fact<bool> {
        let dnf = self == Self::Dnf;
        let (command, args) = if dnf {
            ("rpm", ["-q", "zfs-dkms"].as_slice())
        } else {
            ("dpkg-query", ["-W", "-f=${Status}", "zfs-dkms"].as_slice())
        };
        let query = format!("{command} {}", args.join(" "));
        let output = match Command::new(command).args(args).env("LC_ALL", "C").output() {
            Ok(output) => output,
            Err(error) => return Fact::Unknown(format!("`{query}` couldn't run: {error}")),
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        match output.status.code() {
            Some(0) if dnf => Fact::Known(true),
            Some(0) if let Some(installed) = dpkg_status_installed(&stdout) => {
                Fact::Known(installed)
            }
            // dpkg-query exits 1 only for a package it doesn't know; errors exit 2.
            Some(1) if !dnf => Fact::Known(false),
            Some(1)
                if stdout.trim_end() == "package zfs-dkms is not installed"
                    && stderr.is_empty() =>
            {
                Fact::Known(false)
            }
            _ => Fact::Unknown(format!(
                "`{query}` {}: {} {}",
                output.status,
                stdout.trim(),
                stderr.trim()
            )),
        }
    }

    /// Whether a package ships `build`'s source, by the package manager's own answer.
    pub(super) fn owns(self, build: &ZfsBuild) -> Ownership {
        match build {
            ZfsBuild::Ubuntu { .. } => match deb_installed("zfsutils-linux") {
                Fact::Known(true) => Ownership::Packaged,
                Fact::Known(false) => Ownership::Unpackaged,
                Fact::Unknown(cause) => Ownership::Unknown(cause),
            },
            // rpm exits 1 for any failed query, and prints its negative even when the database
            // didn't open. Only that negative for this path, alone, says no package owns it.
            ZfsBuild::Dkms { source_dir, .. } if self == Self::Dnf => {
                let path = source_dir.display();
                let unowned = [
                    format!("file {path} is not owned by any package"),
                    format!("error: file {path}: No such file or directory"),
                ];
                path_ownership("rpm", "-qf", source_dir, |text| {
                    unowned.iter().any(|line| text.trim_end() == line)
                })
            }
            // dpkg exits 1 only when no package matches; errors exit 2.
            ZfsBuild::Dkms { source_dir, .. } => path_ownership("dpkg", "-S", source_dir, |_| true),
        }
    }
}

/// Whether dpkg leaves `package`'s files on the Machine. Only a status dpkg printed counts as an
/// answer: dpkg-query exits 1 for a package it doesn't know, and anything else, such as exit 2 or
/// a status that isn't `<want> <flag> <state>`, is no answer.
pub(super) fn deb_installed(package: &str) -> Fact<bool> {
    let query = format!("dpkg-query -W -f=${{Status}} {package}");
    let output = match Command::new("dpkg-query")
        .args(["-W", "-f=${Status}", package])
        .output()
    {
        Ok(output) => output,
        Err(error) => return Fact::Unknown(format!("`{query}` couldn't run: {error}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    match output.status.code() {
        Some(0) => dpkg_status_installed(&stdout).map_or_else(
            || Fact::Unknown(format!("`{query}` printed {:?}", stdout.trim())),
            Fact::Known,
        ),
        Some(1) => Fact::Known(false),
        _ => Fact::Unknown(format!("`{query}` {}", output.status)),
    }
}

/// Whether a dpkg `${Status}` (`<want> <flag> <state>`) leaves the package's files on the
/// Machine, or `None` when it isn't one. The want (`hold`, `deinstall`, `purge`) is only a wish
/// and the flag only marks a failed operation, so neither removes anything. A half-installed,
/// unpacked or half-configured package counts too, since apt would still remove it along with
/// its dependencies.
pub(super) fn dpkg_status_installed(status: &str) -> Option<bool> {
    match status.split_whitespace().collect::<Vec<_>>()[..] {
        [_, _, state] => Some(!matches!(state, "not-installed" | "config-files")),
        _ => None,
    }
}

/// Whether an installed package provides `capability`. rpm exits 1 for any failed query, so a
/// refusal to answer is the same as no provider; only an rpm that can't run is no answer.
pub(super) fn rpm_provided(capability: &str) -> Fact<bool> {
    Fact::from(
        Command::new("rpm")
            .args(["-q", "--whatprovides", capability])
            .output()
            .map_err(|error| format!("`rpm -q --whatprovides {capability}` couldn't run: {error}")),
    )
    .map(|output| output.status.success())
}

/// Whether a package ships a build's source, by the package manager's own answer.
pub(super) enum Ownership {
    Packaged,
    Unpackaged,
    /// The query failed, so neither answer is safe to act on.
    Unknown(String),
}

fn path_ownership(
    command: &str,
    flag: &str,
    path: &Path,
    unowned: impl Fn(&str) -> bool,
) -> Ownership {
    let query = format!("{command} {flag} {}", path.display());
    let output = match Command::new(command)
        .arg(flag)
        .arg(path)
        .env("LC_ALL", "C")
        .output()
    {
        Ok(output) => output,
        Err(error) => return Ownership::Unknown(format!("`{query}` couldn't run: {error}")),
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    match output.status.code() {
        Some(0) => Ownership::Packaged,
        Some(1) if unowned(&text) => Ownership::Unpackaged,
        _ => Ownership::Unknown(format!("`{query}` {}: {}", output.status, text.trim())),
    }
}

/// What `dkms status -m zfs` printed, whatever its exit code. DKMS that can't run has built
/// nothing this Machine can use, so it answers no text, after saying why.
pub(super) struct DkmsStatus(String);

impl DkmsStatus {
    /// The status of the ZFS module, narrowed to `version` and `kernel` when given.
    pub(super) fn read(version: Option<&str>, kernel: Option<&str>) -> Self {
        let mut args = vec!["-m", "zfs"];
        args.extend(version.into_iter().flat_map(|version| ["-v", version]));
        args.extend(kernel.into_iter().flat_map(|kernel| ["-k", kernel]));
        Self(
            Fact::from(
                Command::new("dkms")
                    .arg("status")
                    .args(args)
                    .output()
                    .map_err(|error| format!("`dkms status` couldn't run: {error}")),
            )
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .or_report("read the DKMS status of the ZFS build", String::new()),
        )
    }

    /// Whether DKMS knows the module at all, built or not.
    pub(super) fn registered(&self) -> bool {
        !self.0.trim().is_empty()
    }

    pub(super) fn built_or_installed(&self) -> bool {
        self.ends_with(&[": built", ": installed"])
    }

    pub(super) fn installed(&self) -> bool {
        self.ends_with(&[": installed"])
    }

    fn ends_with(&self, states: &[&str]) -> bool {
        self.0.lines().any(|line| {
            let line = line.trim_end();
            states.iter().any(|state| line.ends_with(state))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn os(id: &str, version_id: Option<&str>) -> OsRelease {
        OsRelease {
            id: id.to_owned(),
            name: id.to_owned(),
            version_id: version_id.map(str::to_owned),
        }
    }

    fn argv(command: &Command) -> Vec<OsString> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(OsStr::to_owned)
            .collect()
    }

    #[test]
    fn each_os_family_has_its_package_manager() {
        for (id, version, expected) in [
            ("ubuntu", Some("24.04"), Some(PackageManager::Apt)),
            ("debian", Some("11"), Some(PackageManager::Apt)),
            ("amzn", Some("2023"), Some(PackageManager::Dnf)),
            ("amzn", Some("2"), Some(PackageManager::Dnf)),
            ("fedora", Some("40"), None),
        ] {
            assert_eq!(PackageManager::of(&os(id, version)), expected, "{id}");
        }
    }

    /// The argument arrays every install and removal sends, written once.
    #[test]
    fn install_and_remove_send_the_same_arguments_everywhere() {
        let apt_get = ["apt-get", "-o", "DPkg::Lock::Timeout=300"];
        assert_eq!(
            argv(&PackageManager::Apt.install(["a", "b"])),
            [
                &apt_get[..],
                &["install", "-y", "-qq", "--no-install-recommends", "a", "b"]
            ]
            .concat()
        );
        assert_eq!(
            argv(&PackageManager::Apt.remove(["a"])),
            [&apt_get[..], &["remove", "-y", "-qq", "a"]].concat()
        );
        assert_eq!(
            argv(&PackageManager::Dnf.install(["a", "b"])),
            ["dnf", "install", "-y", "a", "b"]
        );
        assert_eq!(
            argv(&PackageManager::Dnf.remove(["a"])),
            ["dnf", "remove", "-y", "a"]
        );
    }
}
