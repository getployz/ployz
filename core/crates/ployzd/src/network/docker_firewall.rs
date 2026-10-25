//! Whether the running dockerd filters IPv6 with ip6tables, read from its own
//! flags and its `daemon.json`. Docker leaves its ip6tables chains in
//! the kernel when a restart turns ip6tables off, so the chains alone prove
//! nothing until a reboot clears them (#3458). Nor does the file alone: it may
//! have changed since dockerd loaded it (#3463).

use std::{fs, io, os::unix::fs::MetadataExt, path::Path};

const DEFAULT_CONFIG_FILE: &str = "/etc/docker/daemon.json";
const DEFAULT_PIDFILE: &str = "/var/run/docker.pid";

/// Fail-closed: no single root dockerd, an unreadable config, a config changed
/// since dockerd started or a value Docker would reject all count as off.
pub(super) fn docker_filters_ipv6() -> bool {
    dockerd().is_some_and(|(pid, args)| filters_ipv6(&pid, &args, &Host))
}

/// The files `filters_ipv6` looks at, so tests can stand in for the host.
trait Files {
    fn read(&self, path: &Path) -> io::Result<String>;
    /// When the path last changed, in nanoseconds. This is its ctime, which a copy
    /// that keeps the old mtime, such as `rsync -a`, still moves on.
    fn changed(&self, path: &Path) -> io::Result<i128>;
}

struct Host;

impl Files for Host {
    fn read(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn changed(&self, path: &Path) -> io::Result<i128> {
        let metadata = fs::metadata(path)?;
        Ok(i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec()))
    }
}

/// The PID and command line of the one dockerd running as root in this PID namespace.
/// A rootless daemon runs as its user and one inside a container, such as a
/// Docker-in-Docker Service, sits in its own namespace; neither owns the
/// bridge. Two that could leave unknown which one does.
fn dockerd() -> Option<(String, Vec<String>)> {
    let namespace = fs::read_link("/proc/self/ns/pid").ok()?;
    let mut daemons = fs::read_dir("/proc")
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|comm| comm.trim_end() == "dockerd")
                && entry.metadata().is_ok_and(|metadata| metadata.uid() == 0)
                && fs::read_link(entry.path().join("ns/pid")).is_ok_and(|ns| ns == namespace)
        });
    let daemon = daemons.next()?;
    if daemons.next().is_some() {
        return None;
    }
    let args = cmdline_args(&fs::read(daemon.path().join("cmdline")).ok()?)?;
    Some((daemon.file_name().into_string().ok()?, args))
}

/// `/proc/<pid>/cmdline` ends each argument with a NUL. An empty argument, as in
/// `--config-file ""`, is a value and stays. A zombie's is empty: `None`.
fn cmdline_args(cmdline: &[u8]) -> Option<Vec<String>> {
    let cmdline = cmdline.strip_suffix(&[0])?;
    Some(
        cmdline
            .split(|byte| *byte == 0)
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

/// `args` is dockerd's command line, program first, and `pid` its PID. dockerd
/// refuses to start when a flag and the file set the same option, so either
/// source decides it.
fn filters_ipv6(pid: &str, args: &[String], files: &impl Files) -> bool {
    let Some((flags, config_file)) = flag_settings(args.get(1..).unwrap_or_default()) else {
        return false;
    };
    // An empty `--config-file=` loads no file.
    if config_file == Some("") {
        return flags.filters_ipv6();
    }
    let path = Path::new(config_file.unwrap_or(DEFAULT_CONFIG_FILE));
    let (file, changed) = match files.read(path) {
        Ok(contents) => (file_settings(&contents), files.changed(path)),
        // Docker starts without its default file; a named one it must have read.
        // Deleting the file changes its directory, and a missing directory held
        // no file to delete.
        Err(error) if error.kind() == io::ErrorKind::NotFound && config_file.is_none() => {
            let directory = match files.changed(path.parent().unwrap_or(path)) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(i128::MIN),
                changed => changed,
            };
            (Some(Settings::default()), directory)
        }
        Err(_) => return false,
    };
    let Some(settings) = file.map(|file| flags.or(file)) else {
        return false;
    };
    settings.filters_ipv6()
        && changed
            .is_ok_and(|changed| started_since(pid, settings.pidfile.as_deref(), changed, files))
}

/// dockerd writes its pidfile once it has loaded its config, so a config that
/// changed later is one the running dockerd never read (#3463). A pidfile that
/// isn't this dockerd's can't say when it started. A relative one is in
/// dockerd's working directory.
fn started_since(pid: &str, pidfile: Option<&str>, changed: i128, files: &impl Files) -> bool {
    let pidfile = Path::new("/proc")
        .join(pid)
        .join("cwd")
        .join(pidfile.unwrap_or(DEFAULT_PIDFILE));
    let pidfile = pidfile.as_path();
    files
        .read(pidfile)
        .is_ok_and(|contents| contents.trim() == pid)
        && files
            .changed(pidfile)
            .is_ok_and(|started| changed <= started)
}

#[derive(Default)]
struct Settings {
    iptables: Option<bool>,
    ip6tables: Option<bool>,
    firewall_backend: Option<String>,
    pidfile: Option<String>,
}

impl Settings {
    fn or(self, other: Self) -> Self {
        Self {
            iptables: self.iptables.or(other.iptables),
            ip6tables: self.ip6tables.or(other.ip6tables),
            firewall_backend: self.firewall_backend.or(other.firewall_backend),
            pidfile: self.pidfile.or(other.pidfile),
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

/// The firewall flags, the pidfile and `--config-file`, or `None` for a value
/// dockerd would reject. Booleans take only `--flag` or `--flag=value`; strings
/// also take the next argument. `-p` also takes `-pVALUE`, after `-D`.
fn flag_settings(args: &[String]) -> Option<(Settings, Option<&str>)> {
    let mut settings = Settings::default();
    let mut config_file = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let shorthand = arg
            .strip_prefix('-')
            .filter(|arg| !arg.starts_with('-'))
            .map(|arg| arg.trim_start_matches('D'));
        if let Some(value) = shorthand.and_then(|arg| arg.strip_prefix('p')) {
            let value = value.strip_prefix('=').unwrap_or(value);
            let value = match value {
                "" => args.next()?.as_str(),
                value => value,
            };
            settings.pidfile = Some(value.to_owned());
            continue;
        }
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
            "--pidfile" => {
                let value = value.or_else(|| args.next().map(String::as_str))?;
                settings.pidfile = Some(value.to_owned());
            }
            "--config-file" => {
                config_file = Some(value.or_else(|| args.next().map(String::as_str))?);
            }
            _ => {}
        }
    }
    Some((settings, config_file))
}

/// `None` when the file isn't a JSON object or a key read here has a type
/// dockerd would reject. Like dockerd, a byte order mark is dropped and a blank
/// file holds no settings.
fn file_settings(contents: &str) -> Option<Settings> {
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    if contents.trim().is_empty() {
        return Some(Settings::default());
    }
    let serde_json::Value::Object(config) = serde_json::from_str(contents).ok()? else {
        return None;
    };
    let flag = |key: &str| match config.get(key) {
        None => Some(None),
        Some(value) => value.as_bool().map(Some),
    };
    let string = |key: &str| match config.get(key) {
        None | Some(serde_json::Value::Null) => Some(None),
        Some(value) => value.as_str().map(|value| Some(value.to_owned())),
    };
    Some(Settings {
        iptables: flag("iptables")?,
        ip6tables: flag("ip6tables")?,
        firewall_backend: string("firewall-backend")?,
        pidfile: string("pidfile")?,
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
    use std::{cell::RefCell, collections::HashMap, path::PathBuf};

    use super::*;

    const PID: &str = "4242";

    /// Files by path, each with its contents and when it changed. dockerd's
    /// pidfile changed at 100 by default; everything else at 50.
    #[derive(Default)]
    struct Fake {
        files: HashMap<PathBuf, (Option<String>, i128)>,
        read: RefCell<Vec<PathBuf>>,
    }

    impl Fake {
        fn with_pidfile() -> Self {
            Self::default()
                .file(DEFAULT_PIDFILE, &format!("{PID}\n"), 100)
                .dir("/etc/docker", 50)
        }

        fn file(mut self, path: &str, contents: &str, changed: i128) -> Self {
            self.files
                .insert(path.into(), (Some(contents.to_owned()), changed));
            self
        }

        fn dir(mut self, path: &str, changed: i128) -> Self {
            self.files.insert(path.into(), (None, changed));
            self
        }

        fn without(mut self, path: &str) -> Self {
            self.files.remove(Path::new(path));
            self
        }

        fn decide(&self, args: &[&str]) -> bool {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            filters_ipv6(PID, &args, self)
        }
    }

    impl Files for Fake {
        fn read(&self, path: &Path) -> io::Result<String> {
            self.read.borrow_mut().push(path.to_owned());
            match self.files.get(path) {
                Some((Some(contents), _)) => Ok(contents.clone()),
                Some((None, _)) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
                None => Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        }

        fn changed(&self, path: &Path) -> io::Result<i128> {
            self.files
                .get(path)
                .map(|(_, changed)| *changed)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }
    }

    fn decide(args: &[&str], file: Option<&str>) -> bool {
        let fake = Fake::with_pidfile();
        match file {
            Some(contents) => fake.file(DEFAULT_CONFIG_FILE, contents, 50),
            None => fake,
        }
        .decide(args)
    }

    #[test]
    fn docker_defaults_filter_ipv6() {
        assert!(decide(&["/usr/bin/dockerd"], None));
        assert!(decide(&["/usr/bin/dockerd"], Some("{}")));
        assert!(decide(&["dockerd"], Some(r#"{"ip6tables": true}"#)));
        assert!(decide(&["dockerd", "--ip6tables"], Some("{}")));
        assert!(decide(&["dockerd", "--firewall-backend", "iptables"], None));
        assert!(decide(&["dockerd"], Some(" \n")));
        assert!(decide(&["dockerd"], Some("\u{feff}{}")));
        assert!(decide(&["dockerd", "--config-file="], None));
        assert!(!decide(
            &["dockerd", "--config-file=", "--ip6tables=false"],
            None
        ));
        assert!(!decide(
            &["dockerd"],
            Some("\u{feff}{\"ip6tables\": false}")
        ));
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
    fn an_empty_config_file_argument_loads_no_file() {
        let args = cmdline_args(b"/usr/bin/dockerd\0--config-file\0\0").unwrap();
        let fake = Fake::default();
        assert!(filters_ipv6(PID, &args, &fake));
        assert!(fake.read.borrow().is_empty(), "dockerd read no file");
    }

    #[test]
    fn named_config_file_is_read() {
        let fake = Fake::with_pidfile().file("/srv/docker.json", "{}", 50);
        assert!(fake.decide(&["dockerd", "--config-file", "/srv/docker.json"]));
        let fake = fake.file("/srv/docker.json", r#"{"ip6tables": false}"#, 50);
        assert!(!fake.decide(&["dockerd", "--config-file", "/srv/docker.json"]));
        assert_eq!(
            fake.read.borrow().first().map(PathBuf::as_path),
            Some(Path::new("/srv/docker.json"))
        );
    }

    #[test]
    fn a_config_changed_after_dockerd_started_is_unknown() {
        // `"ip6tables": false` was loaded, then removed without a Docker restart.
        let fake = Fake::with_pidfile().file(DEFAULT_CONFIG_FILE, "{}", 150);
        assert!(!fake.decide(&["dockerd"]));
        // A deleted file changed its directory.
        let fake = Fake::with_pidfile()
            .without(DEFAULT_CONFIG_FILE)
            .dir("/etc/docker", 150);
        assert!(!fake.decide(&["dockerd"]));
        // Docker restarted since: its pidfile is newer.
        let fake = Fake::with_pidfile().file(DEFAULT_CONFIG_FILE, "{}", 100);
        assert!(fake.decide(&["dockerd"]));
        // No /etc/docker at all held no file to delete.
        assert!(
            Fake::with_pidfile()
                .without("/etc/docker")
                .decide(&["dockerd"])
        );
    }

    #[test]
    fn the_pidfile_must_be_this_dockerds() {
        let config = |fake: Fake| fake.file(DEFAULT_CONFIG_FILE, "{}", 50);
        assert!(!config(Fake::with_pidfile().without(DEFAULT_PIDFILE)).decide(&["dockerd"]));
        assert!(!config(Fake::with_pidfile().file(DEFAULT_PIDFILE, "1", 100)).decide(&["dockerd"]));
        // A pidfile moved by flag or by daemon.json is the one read.
        let moved = config(Fake::with_pidfile().without(DEFAULT_PIDFILE)).file(
            "/run/dockerd.pid",
            PID,
            100,
        );
        assert!(moved.decide(&["dockerd", "--pidfile", "/run/dockerd.pid"]));
        assert!(moved.decide(&["dockerd", "-p=/run/dockerd.pid"]));
        assert!(moved.decide(&["dockerd", "-p/run/dockerd.pid"]));
        assert!(moved.decide(&["dockerd", "-Dp", "/run/dockerd.pid"]));
        let relative = config(Fake::with_pidfile()).file("/proc/4242/cwd/docker.pid", PID, 100);
        assert!(relative.decide(&["dockerd", "--pidfile=docker.pid"]));
        assert!(config(Fake::with_pidfile()).decide(&["dockerd", "-H", "fd://"]));
        assert!(decide(&["dockerd"], Some(r#"{"pidfile": null}"#)));
        let moved = moved.file(
            DEFAULT_CONFIG_FILE,
            r#"{"pidfile": "/run/dockerd.pid"}"#,
            50,
        );
        assert!(moved.decide(&["dockerd"]));
        assert!(!moved.decide(&["dockerd", "--pidfile"]));
    }

    #[test]
    fn unknown_state_fails_closed() {
        assert_eq!(cmdline_args(b""), None);
        assert!(!decide(
            &["dockerd", "--config-file=/srv/docker.json"],
            None
        ));
        assert!(!decide(&["dockerd"], Some("not json")));
        assert!(!decide(&["dockerd"], Some(r#"{"ip6tables": "yes"}"#)));
        assert!(!decide(&["dockerd", "--ip6tables=maybe"], None));
        assert!(!decide(&["dockerd", "--firewall-backend"], None));
        assert!(
            !Fake::with_pidfile()
                .dir(DEFAULT_CONFIG_FILE, 50)
                .decide(&["dockerd"])
        );
        assert!(!decide(&["dockerd"], Some(r#"{"pidfile": 1}"#)));
    }
}
