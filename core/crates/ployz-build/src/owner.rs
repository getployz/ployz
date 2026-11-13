//! Who held a quarantined build slot, and whether any of it is still running.
//!
//! A slot's lock file carries a mark while an attempt may have work running.
//! The mark names the boot, the process that owns the attempt, and every
//! Docker CLI it spawned, each by PID and start time so a reused PID never
//! counts as the original process.

use std::{fmt, fs, path::Path};

const HEADER: &str = "termination unconfirmed";

/// One process, told apart from any later process reusing its PID.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Process {
    pid: u32,
    start: u64,
}

impl Process {
    pub(crate) fn current() -> std::io::Result<Self> {
        Self::observe(std::process::id())?
            .ok_or_else(|| std::io::Error::other("this process has no /proc entry"))
    }

    /// The running process with `pid`, or `None` once it has exited.
    pub(crate) fn observe(pid: u32) -> std::io::Result<Option<Self>> {
        let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        // The command name may hold spaces and parentheses; fields follow its last `)`.
        let fields = stat
            .rsplit_once(')')
            .map(|(_, rest)| rest.split_whitespace().collect::<Vec<_>>())
            .unwrap_or_default();
        let (Some(state), Some(start)) = (fields.first(), fields.get(19)) else {
            return Err(std::io::Error::other(format!(
                "unreadable /proc/{pid}/stat"
            )));
        };
        if matches!(*state, "Z" | "X") {
            return Ok(None);
        }
        let start = start
            .parse()
            .map_err(|_| std::io::Error::other(format!("unreadable /proc/{pid}/stat")))?;
        Ok(Some(Self { pid, start }))
    }

    fn alive(self) -> std::io::Result<bool> {
        Ok(Self::observe(self.pid)? == Some(self))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Role {
    Owner,
    Docker,
}

impl Role {
    const fn keyword(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Docker => "docker",
        }
    }
}

/// What a quarantined slot's lock file says about the attempt that left it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// Written before marks named their processes.
    Unnamed,
    Named {
        boot: String,
        processes: Vec<(Role, Process)>,
    },
}

impl Mark {
    /// The opening of a mark owned by this process.
    pub(crate) fn header() -> std::io::Result<String> {
        let owner = Process::current()?;
        Ok(format!(
            "{HEADER}\nboot {}\n{}",
            boot_id()?,
            line(Role::Owner, owner)
        ))
    }

    pub(crate) fn docker_line(process: Process) -> String {
        line(Role::Docker, process)
    }

    pub(crate) fn parse(text: &str) -> Self {
        let mut boot = None;
        let mut processes = Vec::new();
        for words in text.lines().map(|line| line.split(' ').collect::<Vec<_>>()) {
            match words.as_slice() {
                ["boot", id] => boot = Some((*id).to_owned()),
                [role @ ("owner" | "docker"), pid, start] => {
                    let (Ok(pid), Ok(start)) = (pid.parse(), start.parse()) else {
                        continue;
                    };
                    let role = if *role == "owner" {
                        Role::Owner
                    } else {
                        Role::Docker
                    };
                    processes.push((role, Process { pid, start }));
                }
                _ => {}
            }
        }
        match boot {
            Some(boot) => Self::Named { boot, processes },
            None => Self::Unnamed,
        }
    }

    /// Every process of the marked attempt that is still running, plus any
    /// Buildx process still naming `builder`.
    ///
    /// # Errors
    /// Fails when `/proc` cannot be read, since nothing is then known.
    pub(crate) fn survivors(&self, builder: &str) -> std::io::Result<Vec<Survivor>> {
        let mut survivors = Vec::new();
        if let Self::Named { boot, processes } = self
            && *boot == boot_id()?
        {
            for &(role, process) in processes {
                if process.alive()? && !survivors.contains(&Survivor::Recorded(role, process)) {
                    survivors.push(Survivor::Recorded(role, process));
                }
            }
        }
        for pid in buildx_processes(builder)? {
            if !survivors.iter().any(|survivor| survivor.pid() == pid) {
                survivors.push(Survivor::Buildx(pid));
            }
        }
        Ok(survivors)
    }
}

/// A mark naming `processes` on this boot.
#[cfg(test)]
pub(crate) fn named(processes: &[(Role, Process)]) -> String {
    let mut mark = format!("{HEADER}\nboot {}\n", boot_id().unwrap());
    for &(role, process) in processes {
        mark.push_str(&line(role, process));
    }
    mark
}

fn line(role: Role, process: Process) -> String {
    format!("{} {} {}\n", role.keyword(), process.pid, process.start)
}

/// A process that keeps a slot quarantined.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Survivor {
    Recorded(Role, Process),
    Buildx(u32),
}

impl Survivor {
    fn pid(&self) -> u32 {
        match self {
            Self::Recorded(_, process) => process.pid,
            Self::Buildx(pid) => *pid,
        }
    }
}

impl fmt::Display for Survivor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Recorded(Role::Owner, process) => {
                write!(f, "process {} that owned the Build", process.pid)
            }
            Self::Recorded(Role::Docker, process) => {
                write!(f, "Docker CLI process {} the Build started", process.pid)
            }
            Self::Buildx(pid) => write!(f, "Buildx process {pid} using the builder"),
        }
    }
}

fn boot_id() -> std::io::Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

/// Buildx processes, other than this one, whose arguments name `builder`.
fn buildx_processes(builder: &str) -> std::io::Result<Vec<u32>> {
    let own = std::process::id();
    let flag = format!("--builder={builder}");
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let Some(pid) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        // A process that exits mid-scan, or another user's, cannot be one of ours.
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let arguments = cmdline
            .split(|&byte| byte == 0)
            .map(String::from_utf8_lossy)
            .collect::<Vec<_>>();
        let buildx = arguments.iter().enumerate().any(|(index, argument)| {
            argument == "buildx"
                || (index == 0
                    && Path::new(argument.as_ref())
                        .file_name()
                        .is_some_and(|name| name == "docker-buildx"))
        });
        if buildx
            && arguments
                .iter()
                .any(|argument| argument == builder || *argument == flag)
            && Process::observe(pid).is_ok_and(|process| process.is_some())
        {
            found.push(pid);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_without_a_boot_names_no_processes() {
        assert_eq!(Mark::parse("termination unconfirmed\n"), Mark::Unnamed);
        assert_eq!(
            Mark::parse("termination unconfirmed\ntermination unconfirmed\n"),
            Mark::Unnamed
        );
    }

    #[test]
    fn this_process_survives_and_a_reused_pid_does_not() {
        let mark = Mark::parse(&Mark::header().unwrap());
        let Mark::Named { processes, .. } = &mark else {
            panic!("a fresh mark names its owner");
        };
        let [(Role::Owner, owner)] = processes.as_slice() else {
            panic!("a fresh mark names only its owner: {processes:?}");
        };
        let owner = *owner;
        assert_eq!(
            mark.survivors("ployz-test-no-such-builder").unwrap(),
            vec![Survivor::Recorded(Role::Owner, owner)]
        );
        let reused = Process {
            start: owner.start + 1,
            ..owner
        };
        let mark = Mark::parse(&format!(
            "{HEADER}\nboot {}\n{}",
            boot_id().unwrap(),
            line(Role::Owner, reused)
        ));
        assert!(
            mark.survivors("ployz-test-no-such-builder")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_mark_from_another_boot_has_no_survivors() {
        let mark = Mark::parse(&format!(
            "{HEADER}\nboot another-boot\n{}",
            line(Role::Owner, Process::current().unwrap())
        ));
        assert!(
            mark.survivors("ployz-test-no-such-builder")
                .unwrap()
                .is_empty()
        );
    }
}
