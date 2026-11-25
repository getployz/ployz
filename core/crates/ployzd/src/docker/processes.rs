//! A Container's processes as the kernel lists them, so evidence that an exec's command
//! ended rests on nothing Docker reports. Docker starts a Container's process and each exec
//! in it in the Container's cgroup, from outside it: runc starts the process, and
//! containerd's shim takes it as its child once runc exits.

use std::{collections::HashSet, io};

/// One process, told apart from a later one the kernel gives the same PID.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Process {
    pid: i32,
    start: u64,
}

/// What `/proc/<pid>/stat` says of a running process.
struct Stat {
    process: Process,
    parent: i32,
}

impl Stat {
    /// The process at `pid`, `None` once it exited.
    fn read(pid: i32) -> io::Result<Option<Self>> {
        let Some(stat) = read_live(pid, "stat")? else {
            return Ok(None);
        };
        let unreadable = || io::Error::other(format!("unreadable /proc/{pid}/stat"));
        // The command name may hold spaces and parentheses; fields follow its last `)`.
        let fields = stat
            .rsplit_once(')')
            .map(|(_, rest)| rest.split_whitespace().collect::<Vec<_>>())
            .unwrap_or_default();
        let (Some(state), Some(parent), Some(start)) =
            (fields.first(), fields.get(1), fields.get(19))
        else {
            return Err(unreadable());
        };
        if matches!(*state, "Z" | "X") {
            return Ok(None);
        }
        Ok(Some(Self {
            process: Process {
                pid,
                start: start.parse().map_err(|_| unreadable())?,
            },
            parent: parent.parse().map_err(|_| unreadable())?,
        }))
    }
}

/// `/proc/<pid>/<file>`, `None` once that process exited.
fn read_live(pid: i32, file: &str) -> io::Result<Option<String>> {
    match std::fs::read_to_string(format!("/proc/{pid}/{file}")) {
        Ok(read) => Ok(Some(read)),
        // A process exiting mid-read answers ESRCH.
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// A Container's cgroup, read before an exec in it starts, with the processes started into it
/// from outside by then: the Container's process and the execs already running.
pub(super) struct Cgroup {
    /// `/proc/<pid>/cgroup` of the Container's process.
    path: String,
    /// The PID of the Container process's parent, containerd's shim.
    shim: i32,
    before: HashSet<Process>,
}

impl Cgroup {
    /// The cgroup of the Container whose process runs at `init`.
    ///
    /// # Errors
    /// `/proc` can't be read, or the Container's process exited.
    pub(super) fn read(init: u32) -> io::Result<Self> {
        let init = i32::try_from(init).map_err(io::Error::other)?;
        let exited = || io::Error::other("the Container's process exited");
        let path = read_live(init, "cgroup")?.ok_or_else(exited)?;
        let shim = Stat::read(init)?.ok_or_else(exited)?.parent;
        let mut cgroup = Self {
            path,
            shim,
            before: HashSet::new(),
        };
        cgroup.before = cgroup.started_from_outside()?;
        Ok(cgroup)
    }

    /// Whether the process at `pid` runs in this cgroup, `None` once it exited.
    ///
    /// # Errors
    /// Its `/proc` entry can't be read.
    pub(super) fn holds(&self, pid: i32) -> io::Result<Option<bool>> {
        Ok(read_live(pid, "cgroup")?.map(|read| read == self.path))
    }

    /// Whether a process in the cgroup may be an exec Docker started since [`Self::read`]: one
    /// started into it from outside that wasn't there then.
    ///
    /// # Errors
    /// `/proc` can't be read.
    pub(super) fn may_hold_new_exec(&self) -> io::Result<bool> {
        Ok(!self.started_from_outside()?.is_subset(&self.before))
    }

    /// Each process in the cgroup whose parent is the shim or runs outside the cgroup, as an
    /// exec's is while runc starts it, once the shim took it, and once the shim is gone.
    fn started_from_outside(&self) -> io::Result<HashSet<Process>> {
        let members = self.members()?;
        let inside: HashSet<i32> = members.iter().map(|stat| stat.process.pid).collect();
        Ok(members
            .into_iter()
            .filter(|stat| stat.parent == self.shim || !inside.contains(&stat.parent))
            .map(|stat| stat.process)
            .collect())
    }

    /// Each process running in the cgroup.
    fn members(&self) -> io::Result<Vec<Stat>> {
        let mut members = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            let Some(pid) = entry?
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            if self.holds(pid)? == Some(true)
                && let Some(stat) = Stat::read(pid)?
            {
                members.push(stat);
            }
        }
        Ok(members)
    }
}
