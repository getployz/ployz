//! Whether the running dockerd filters IPv6 with ip6tables, read from its own
//! flags and the `daemon.json` it loaded. Docker leaves its ip6tables chains in
//! the kernel when a restart turns ip6tables off, so the chains alone prove
//! nothing until a reboot clears them (#3458).

use std::{fs, io, os::unix::fs::MetadataExt, path::Path};

const DEFAULT_CONFIG_FILE: &str = "/etc/docker/daemon.json";

/// Fail-closed: no single root dockerd, an unreadable config or a value Docker
/// would reject all count as off.
pub(super) fn docker_filters_ipv6() -> bool {
    dockerd_args().is_some_and(|args| filters_ipv6(&args, |path| fs::read_to_string(path)))
}

/// The command line of the one dockerd running as root. A rootless daemon runs
/// as its user and doesn't own the bridge; two root daemons leave unknown which
/// one does.
fn dockerd_args() -> Option<Vec<String>> {
    let mut daemons = fs::read_dir("/proc")
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|comm| comm.trim_end() == "dockerd")
                && entry.metadata().is_ok_and(|metadata| metadata.uid() == 0)
        });
    let daemon = daemons.next()?;
    if daemons.next().is_some() {
        return None;
    }
    let cmdline = fs::read(daemon.path().join("cmdline")).ok()?;
    Some(
        cmdline
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

/// `args` is dockerd's command line, program first. dockerd refuses to start
/// when a flag and the file set the same option, so either source decides it.
fn filters_ipv6(args: &[String], read: impl FnOnce(&Path) -> io::Result<String>) -> bool {
    let Some((flags, config_file)) = flag_settings(args.get(1..).unwrap_or_default()) else {
        return false;
    };
    let file = match read(Path::new(config_file.unwrap_or(DEFAULT_CONFIG_FILE))) {
        Ok(contents) => file_settings(&contents),
        // Docker starts without its default file; a named one it must have read.
        Err(error) if error.kind() == io::ErrorKind::NotFound && config_file.is_none() => {
            Some(Settings::default())
        }
        Err(_) => None,
    };
    file.is_some_and(|file| flags.or(file).filters_ipv6())
}

#[derive(Default)]
struct Settings {
    iptables: Option<bool>,
    ip6tables: Option<bool>,
    firewall_backend: Option<String>,
}

impl Settings {
    fn or(self, other: Self) -> Self {
        Self {
            iptables: self.iptables.or(other.iptables),
            ip6tables: self.ip6tables.or(other.ip6tables),
            firewall_backend: self.firewall_backend.or(other.firewall_backend),
        }
    }

    /// Docker 29 turns ip6tables on by default, and only with iptables, whose
    /// backend is iptables unless set to nftables.
    fn filters_ipv6(&self) -> bool {
        self.iptables.unwrap_or(true)
            && self.ip6tables.unwrap_or(true)
            && matches!(
                self.firewall_backend.as_deref(),
                None | Some("" | "iptables")
            )
    }
}

/// The firewall flags and `--config-file`, or `None` for a value dockerd would
/// reject. Booleans take only `--flag` or `--flag=value`; strings also take the
/// next argument.
fn flag_settings(args: &[String]) -> Option<(Settings, Option<&str>)> {
    let mut settings = Settings::default();
    let mut config_file = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg.as_str(), None),
        };
        match name {
            "--iptables" => settings.iptables = Some(value.map_or(Some(true), go_bool)?),
            "--ip6tables" => settings.ip6tables = Some(value.map_or(Some(true), go_bool)?),
            "--firewall-backend" => {
                let value = value.or_else(|| args.next().map(String::as_str))?;
                settings.firewall_backend = Some(value.to_owned());
            }
            "--config-file" => {
                config_file = Some(value.or_else(|| args.next().map(String::as_str))?);
            }
            _ => {}
        }
    }
    Some((settings, config_file))
}

/// `None` when the file isn't a JSON object or a firewall key has a type
/// dockerd would reject.
fn file_settings(contents: &str) -> Option<Settings> {
    let serde_json::Value::Object(config) = serde_json::from_str(contents).ok()? else {
        return None;
    };
    let flag = |key: &str| match config.get(key) {
        None => Some(None),
        Some(value) => value.as_bool().map(Some),
    };
    Some(Settings {
        iptables: flag("iptables")?,
        ip6tables: flag("ip6tables")?,
        firewall_backend: match config.get("firewall-backend") {
            None => None,
            Some(value) => Some(value.as_str()?.to_owned()),
        },
    })
}

/// Go's `strconv.ParseBool`, which dockerd's flags use.
fn go_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decide(args: &[&str], file: Option<&str>) -> bool {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        filters_ipv6(&args, |_| {
            file.map(str::to_owned)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        })
    }

    #[test]
    fn docker_defaults_filter_ipv6() {
        assert!(decide(&["/usr/bin/dockerd"], None));
        assert!(decide(&["/usr/bin/dockerd"], Some("{}")));
        assert!(decide(&["dockerd"], Some(r#"{"ip6tables": true}"#)));
        assert!(decide(&["dockerd", "--ip6tables"], Some("{}")));
        assert!(decide(&["dockerd", "--firewall-backend", "iptables"], None));
    }

    #[test]
    fn daemon_json_turns_ipv6_filtering_off() {
        for file in [
            r#"{"ip6tables": false}"#,
            r#"{"iptables": false}"#,
            r#"{"firewall-backend": "nftables"}"#,
        ] {
            assert!(!decide(&["dockerd"], Some(file)), "{file}");
        }
    }

    #[test]
    fn flags_turn_ipv6_filtering_off() {
        for flag in [
            &["--ip6tables=false"][..],
            &["--iptables=0"],
            &["--firewall-backend=nftables"],
            &["--firewall-backend", "nftables"],
        ] {
            let args = [&["dockerd"], flag].concat();
            assert!(!decide(&args, Some("{}")), "{flag:?}");
        }
    }

    #[test]
    fn named_config_file_is_read() {
        let args = ["dockerd", "--config-file", "/srv/docker.json"]
            .map(str::to_owned)
            .to_vec();
        let mut read_path = None;
        assert!(!filters_ipv6(&args, |path| {
            read_path = Some(path.to_owned());
            Ok(r#"{"ip6tables": false}"#.to_owned())
        }));
        assert_eq!(read_path.as_deref(), Some(Path::new("/srv/docker.json")));
    }

    #[test]
    fn unknown_state_fails_closed() {
        assert!(!decide(
            &["dockerd", "--config-file=/srv/docker.json"],
            None
        ));
        assert!(!decide(&["dockerd"], Some("not json")));
        assert!(!decide(&["dockerd"], Some(r#"{"ip6tables": "yes"}"#)));
        assert!(!decide(&["dockerd", "--ip6tables=maybe"], None));
        assert!(!decide(&["dockerd", "--firewall-backend"], None));
        assert!(!filters_ipv6(&["dockerd".to_owned()], |_| Err(
            io::Error::from(io::ErrorKind::PermissionDenied)
        )));
    }
}
