//! Which coding agent is driving this process, and the session its calls belong to.
//!
//! An agent is named by the markers harnesses put in the environment, or by an ancestor
//! process with a known name. Detection never fails a command: anything unreadable reads
//! as no agent.

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// The coding agent behind this process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Agent {
    pub(crate) name: String,
    /// The agent's own process, when an ancestor named it.
    process: Option<Process>,
}

/// A process by its id and start time, which together never name another process on
/// this boot.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Process {
    pid: u32,
    started: String,
}

/// An ancestor of this process: its identity and the names it goes by.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ancestor {
    process: Process,
    names: Vec<String>,
}

/// Environment markers harnesses set, and the agent each names. `AI_AGENT` carries the
/// agent's own name.
const MARKERS: &[(&str, &str)] = &[
    ("CLAUDECODE", "claude-code"),
    ("CURSOR_AGENT", "cursor"),
    ("CODEX_THREAD_ID", "codex"),
    ("GEMINI_CLI", "gemini-cli"),
    ("OPENCODE", "opencode"),
    ("AMP_CURRENT_THREAD_ID", "amp"),
    ("DEVIN_DIR", "devin"),
    ("DEVIN_DISABLE_HISTEXPAND", "devin"),
];

/// Process names agents run under, and the agent each names.
const PROCESSES: &[(&str, &str)] = &[
    ("claude", "claude-code"),
    ("codex", "codex"),
    ("cursor-agent", "cursor"),
    ("gemini", "gemini-cli"),
    ("agy", "gemini-cli"),
    ("opencode", "opencode"),
    ("amp", "amp"),
    ("aider", "aider"),
    ("copilot", "copilot"),
    ("devin", "devin"),
];

/// The agent behind this process, read once.
pub(crate) fn detected() -> Option<&'static Agent> {
    static AGENT: OnceLock<Option<Agent>> = OnceLock::new();
    AGENT
        .get_or_init(|| detect(|name| std::env::var(name).ok(), &ancestors()))
        .as_ref()
}

/// The agent's name alone, for the request header Cloud reads.
pub(crate) fn name() -> Option<&'static str> {
    detected().map(|agent| agent.name.as_str())
}

fn detect(var: impl Fn(&str) -> Option<String>, ancestors: &[Ancestor]) -> Option<Agent> {
    let process = ancestors
        .iter()
        .find_map(|ancestor| named_agent(&ancestor.names).map(|name| (name, &ancestor.process)));
    let marked = from_markers(var);
    match (marked, process) {
        (Some(name), process) => Some(Agent {
            name,
            process: process.map(|(_, process)| process.clone()),
        }),
        (None, Some((name, process))) => Some(Agent {
            name: name.to_owned(),
            process: Some(process.clone()),
        }),
        (None, None) => None,
    }
}

/// The agent the environment names: `AI_AGENT` itself, else the first marker set.
fn from_markers(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(agent) = var("AI_AGENT").filter(|agent| !agent.is_empty()) {
        return Some(agent);
    }
    MARKERS
        .iter()
        .find(|(marker, _)| var(marker).is_some_and(|value| !value.is_empty()))
        .map(|(_, agent)| (*agent).to_owned())
}

fn named_agent(names: &[String]) -> Option<&'static str> {
    PROCESSES
        .iter()
        .find(|(process, _)| names.iter().any(|name| name == process))
        .map(|(_, agent)| *agent)
}

/// `ses_` and 32 hex digits of SHA-256 over the agent's process and this boot, so every
/// command the same agent session runs lands in one session. Without an agent process,
/// the parent process stands in. Nothing hashed is a credential.
pub(crate) fn cli_session() -> String {
    let ancestors = ancestors();
    let anchor = detected()
        .and_then(|agent| agent.process.clone())
        .or_else(|| ancestors.first().map(|parent| parent.process.clone()));
    let (pid, started) =
        anchor.map_or((0, String::new()), |process| (process.pid, process.started));
    let digest = Sha256::digest(format!("{pid}:{started}:{}", boot_id()));
    format!("ses_{}", &hex::encode(digest)[..32])
}

#[cfg(target_os = "linux")]
fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|id| id.trim().to_owned())
        .unwrap_or_default()
}

#[cfg(not(target_os = "linux"))]
fn boot_id() -> String {
    std::process::Command::new("sysctl")
        .args(["-n", "kern.bootsessionuuid"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// This process's ancestors, nearest first, as far as they can be read.
#[cfg(target_os = "linux")]
fn ancestors() -> Vec<Ancestor> {
    let mut found = Vec::new();
    let mut pid = parent_of(std::process::id());
    while let Some(Stat {
        ppid,
        started,
        comm,
    }) = pid.and_then(stat)
        && found.len() < 64
    {
        let mut names = vec![comm];
        names.extend(cmdline_names(pid.unwrap_or_default()));
        found.push(Ancestor {
            process: Process {
                pid: pid.unwrap_or_default(),
                started,
            },
            names,
        });
        pid = (ppid > 1).then_some(ppid);
    }
    found
}

#[cfg(target_os = "linux")]
struct Stat {
    ppid: u32,
    started: String,
    comm: String,
}

#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    stat(pid).map(|stat| stat.ppid).filter(|ppid| *ppid > 1)
}

/// `/proc/<pid>/stat`: the name in parentheses, then the parent pid as field 4 and the
/// start time as field 22.
#[cfg(target_os = "linux")]
fn stat(pid: u32) -> Option<Stat> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (before, after) = text.rsplit_once(')')?;
    let comm = before.split_once('(')?.1.to_owned();
    let fields: Vec<&str> = after.split_whitespace().collect();
    Some(Stat {
        ppid: fields.get(1)?.parse().ok()?,
        started: (*fields.get(19)?).to_owned(),
        comm,
    })
}

/// The program names on a process's command line: the first two arguments by their
/// last path segment, so `node /usr/lib/node_modules/agent/cli.js` reads as `node` and
/// `agent`, and `/usr/bin/claude` as `claude`.
#[cfg(target_os = "linux")]
fn cmdline_names(pid: u32) -> Vec<String> {
    let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return Vec::new();
    };
    bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .take(2)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .map(|arg| program_name(&arg))
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn ancestors() -> Vec<Ancestor> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,lstart=,comm="])
        .output()
        .ok()
        .filter(|output| output.status.success());
    let Some(output) = output else {
        return Vec::new();
    };
    let table: Vec<(u32, u32, String, String)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let ppid = words.next()?.parse().ok()?;
            let started = words.by_ref().take(5).collect::<Vec<_>>().join(" ");
            let comm = words.collect::<Vec<_>>().join(" ");
            Some((pid, ppid, started, comm))
        })
        .collect();
    let row = |pid: u32| table.iter().find(|row| row.0 == pid);
    let mut found = Vec::new();
    let mut pid = row(std::process::id()).map(|row| row.1);
    while let Some((pid_now, ppid, started, comm)) = pid.and_then(row)
        && *pid_now > 1
        && found.len() < 64
    {
        found.push(Ancestor {
            process: Process {
                pid: *pid_now,
                started: started.clone(),
            },
            names: vec![program_name(comm)],
        });
        pid = Some(*ppid);
    }
    found
}

/// A program by its last path segment, with a node-style version suffix left alone.
fn program_name(arg: &str) -> String {
    arg.rsplit('/').next().unwrap_or(arg).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    fn ancestor(pid: u32, names: &[&str]) -> Ancestor {
        Ancestor {
            process: Process {
                pid,
                started: format!("{pid}00"),
            },
            names: names.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    #[test]
    fn markers_name_the_agent_and_an_ancestor_names_its_process() {
        assert_eq!(detect(only(&[]), &[]), None);
        assert_eq!(detect(only(&[("CLAUDECODE", "")]), &[]), None);
        assert_eq!(
            detect(only(&[("CLAUDECODE", "1")]), &[]),
            Some(Agent {
                name: "claude-code".to_owned(),
                process: None
            })
        );
        assert_eq!(
            detect(only(&[("CODEX_THREAD_ID", "t1"), ("AI_AGENT", "pi")]), &[])
                .map(|agent| agent.name)
                .as_deref(),
            Some("pi")
        );
        assert_eq!(
            detect(only(&[("DEVIN_DIR", "/opt/.devin")]), &[])
                .map(|agent| agent.name)
                .as_deref(),
            Some("devin")
        );
        // A shell under Claude Code with no marker: the process tree names it.
        let tree = [
            ancestor(40, &["bash"]),
            ancestor(30, &["claude"]),
            ancestor(1, &["systemd"]),
        ];
        assert_eq!(
            detect(only(&[]), &tree),
            Some(Agent {
                name: "claude-code".to_owned(),
                process: Some(Process {
                    pid: 30,
                    started: "3000".to_owned()
                })
            })
        );
        // A node-based agent reads by its script's name.
        let node = [ancestor(50, &["node", "gemini"])];
        assert_eq!(
            detect(only(&[]), &node).map(|agent| agent.name).as_deref(),
            Some("gemini-cli")
        );
    }

    #[test]
    fn the_session_is_a_stable_prefixed_hash() {
        let session = cli_session();
        assert!(session.starts_with("ses_"), "{session}");
        assert_eq!(session.len(), 36, "{session}");
        assert_eq!(session, cli_session());
    }
}
