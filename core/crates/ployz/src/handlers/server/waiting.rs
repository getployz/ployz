//! Servers waiting to join: hosts that checked in to Cloud with a fleet token and wait for
//! someone to enroll or reject them.

use chrono::{DateTime, Utc};
use clap::ArgMatches;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::cloud_runs;
use crate::cloud_account::{self, Credential};
use crate::cloud_login::LoginError;
use crate::handlers::{Error, runtime};
use crate::ui::{self, Cell, Fields, Table, Tone};

/// One waiting server: the row Cloud sent, which `--json` prints, and the fields read from it.
pub(super) struct Waiting {
    pub(super) raw: Value,
    pub(super) row: Row,
}

#[derive(Deserialize)]
pub(super) struct Row {
    pub(super) id: String,
    pub(super) name: String,
    status: Status,
    observed_ip: String,
    pub(super) fleet_token: String,
    report: Report,
    first_seen_at: String,
    last_check_in_at: String,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Offline,
    Blocked,
    Preparing,
    Waiting,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    public_ip: Option<String>,
    provider: Option<String>,
    os: Os,
    arch: String,
    virt: String,
    cpu_count: u64,
    memory_total_bytes: u64,
    disks: Vec<Disk>,
    docker: Docker,
    zfs: Zfs,
    blockers: Vec<Issue>,
    warnings: Vec<Issue>,
    prep: Option<Prep>,
}

#[derive(Deserialize)]
struct Os {
    id: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Disk {
    path: String,
    size_bytes: u64,
    role: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Docker {
    Absent,
    Present {
        version: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Zfs {
    Eligible {
        targets: Vec<ZfsTarget>,
    },
    Ineligible {
        message: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ZfsTarget {
    MainDisk,
    Disk {
        #[serde(rename = "byId")]
        by_id: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct Issue {
    message: String,
}

#[derive(Deserialize)]
struct Prep {
    docker: String,
    zfs: String,
}

/// The Organization's waiting servers, oldest first.
pub(super) async fn fetch(credential: &Credential) -> Result<Vec<Waiting>, Error> {
    #[derive(Deserialize)]
    struct Reply {
        waiting_servers: Vec<Value>,
    }
    let Reply { waiting_servers } =
        cloud_account::refusable(credential, Method::GET, "waiting-servers", None).await?;
    waiting_servers
        .into_iter()
        .map(|raw| {
            let row = serde_json::from_value(printable(raw.clone()))?;
            Ok(Waiting { raw, row })
        })
        .collect()
}

/// `value` with every control character in its strings replaced. A host names itself, so
/// an escape sequence in its report must not reach the terminal.
fn printable(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(
            text.chars()
                .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(printable).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, printable(value)))
                .collect(),
        ),
        other @ (Value::Null | Value::Bool(_) | Value::Number(_)) => other,
    }
}

/// Cloud takes at most this many ids in one reject.
const REJECT_BATCH: usize = 1000;

/// Reject `ids`, answering how many Cloud rejected now.
pub(super) async fn reject(credential: &Credential, ids: &[&str]) -> Result<u64, Error> {
    #[derive(Deserialize)]
    struct Reply {
        rejected: u64,
    }
    let mut total = 0;
    for batch in ids.chunks(REJECT_BATCH) {
        let body = json!({ "ids": batch });
        let Reply { rejected } =
            cloud_account::refusable(credential, Method::POST, "reject-servers", Some(&body))
                .await
                .map_err(|error| {
                    match total {
            0 => Error::from(error),
            done => Error::from(error).context(format!(
                "Rejected {done}; the outcome of the rest is unknown. Run the same reject again."
            )),
        }
                })?;
        total += rejected;
    }
    Ok(total)
}

/// The credential a waiting-list command runs under. The list lives in Cloud alone, so
/// `--context` and `--connect` are refused and signing out asks for `ployz login`.
pub(super) fn credential(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
) -> Result<Credential, Error> {
    if matches.get_one::<String>("connect").is_some()
        || matches.get_one::<String>("context").is_some()
    {
        return Err(Error::usage(
            "servers waiting to join live in Ployz Cloud; drop --context and --connect",
        ));
    }
    cloud_runs(runtime, matches)?.ok_or_else(|| LoginError::SignedOut.into())
}

/// `server ls --waiting`: one fixed row per waiting server.
pub(super) fn list(matches: &ArgMatches) -> Result<(), Error> {
    let runtime = runtime()?;
    let credential = credential(&runtime, matches)?;
    let waiting = runtime.block_on(fetch(&credential))?;
    let now = Utc::now();
    let mut table = Table::new(
        ["NAME", "PROVIDER", "IP", "STATUS", "AGE"],
        "No servers waiting.",
    );
    for Waiting { row, .. } in &waiting {
        let (status, tone) = row.readiness();
        table.row([
            Cell::from(row.name.clone()),
            Cell::from(row.provider().to_owned()),
            Cell::from(row.ip().to_owned()),
            Cell::status(status, tone),
            Cell::from(age(&row.first_seen_at, now)),
        ]);
    }
    let raw: Vec<&Value> = waiting.iter().map(|waiting| &waiting.raw).collect();
    ui::list(&json!({ "waiting_servers": raw }), &table)
}

/// `server show` for a waiting server: its facts, then what keeps it from joining.
pub(super) fn show(waiting: &Waiting) -> Result<(), Error> {
    let row = &waiting.row;
    let report = &row.report;
    let now = Utc::now();
    let (status, _) = row.readiness();
    let mut record = Fields::new()
        .field("waiting server", &row.name)
        .field("status", status)
        .field(
            "machine",
            format!(
                "{} {} · {} · {} CPU · {} · {}",
                report.os.id,
                report.os.version,
                report.arch,
                report.cpu_count,
                size(report.memory_total_bytes),
                report.virt
            ),
        )
        .field("disks", row.disks())
        .field("docker", row.docker())
        .field("storage", row.storage())
        .field(
            "from",
            format!("{} · {} · {}", row.from(), row.provider(), row.fleet_token),
        )
        .field(
            "first seen",
            format!("{} ago", age(&row.first_seen_at, now)),
        )
        .field(
            "last check-in",
            format!("{} ago", age(&row.last_check_in_at, now)),
        )
        .field("id", &row.id);
    for blocker in &report.blockers {
        record.push("blocked", &blocker.message);
    }
    for warning in &report.warnings {
        record.push("warning", &warning.message);
    }
    ui::fields(&json!({ "waiting_server": waiting.raw }), &record)
}

impl Row {
    fn provider(&self) -> &str {
        self.report.provider.as_deref().unwrap_or("Unknown")
    }

    /// The address Cloud saw the host check in from.
    pub(super) fn ip(&self) -> &str {
        &self.observed_ip
    }

    /// The observed address, and the one the host reports when it differs.
    fn from(&self) -> String {
        match self.report.public_ip.as_deref() {
            Some(reported) if reported != self.observed_ip => {
                format!("{} (reports {reported})", self.observed_ip)
            }
            _ => self.observed_ip.clone(),
        }
    }

    fn readiness(&self) -> (&'static str, Tone) {
        let prep = self.report.prep.as_ref();
        let docker_failed = prep.is_some_and(|prep| prep.docker == "failed");
        match self.status {
            Status::Offline => ("Offline", Tone::Bad),
            Status::Blocked | Status::Preparing if docker_failed => {
                ("Docker install failed", Tone::Bad)
            }
            Status::Blocked => ("Blocked", Tone::Bad),
            Status::Preparing => ("Installing Docker", Tone::Change),
            Status::Waiting if prep.is_some_and(|prep| prep.zfs == "installing") => {
                ("Building ZFS", Tone::Change)
            }
            Status::Waiting => ("Ready", Tone::Good),
            Status::Unknown => ("Unknown", Tone::Muted),
        }
    }

    fn disks(&self) -> String {
        self.report
            .disks
            .iter()
            .map(|disk| format!("{} {} {}", disk.path, size(disk.size_bytes), disk.role))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn docker(&self) -> String {
        match &self.report.docker {
            Docker::Present { version } => version.clone(),
            Docker::Absent => "not installed".into(),
            Docker::Unknown => "unknown".into(),
        }
    }

    fn storage(&self) -> String {
        if self
            .report
            .prep
            .as_ref()
            .is_some_and(|prep| prep.zfs == "failed")
        {
            return "Docker volumes; the ZFS build failed".into();
        }
        match &self.report.zfs {
            Zfs::Eligible { targets } => {
                let on = targets
                    .iter()
                    .map(|target| match target {
                        ZfsTarget::MainDisk => "the main disk".to_owned(),
                        ZfsTarget::Disk { by_id } => by_id.clone(),
                        ZfsTarget::Unknown => "another disk".to_owned(),
                    })
                    .collect::<Vec<_>>();
                format!("ZFS on {}", on.join(" or "))
            }
            Zfs::Ineligible { message } => format!("Docker volumes; {message}"),
            Zfs::Unknown => "unknown".into(),
        }
    }
}

/// Time since `at`, in its largest whole unit.
fn age(at: &str, now: DateTime<Utc>) -> String {
    let Ok(at) = DateTime::parse_from_rfc3339(at) else {
        return "-".into();
    };
    let seconds = (now - at.with_timezone(&Utc)).num_seconds().max(0);
    match seconds {
        ..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..172_800 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Bytes in decimal GB, as storage limits are written.
fn size(bytes: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "display rounding only")]
    let gigabytes = bytes as f64 / 1e9;
    format!("{gigabytes:.0} GB")
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(crate) fn waiting(status: &str, prep: Option<Value>) -> Waiting {
        let mut report = json!({
            "hostname": "web-1",
            "provider": "hetzner",
            "os": { "id": "ubuntu", "version": "24.04" },
            "kernel": "6.8.0",
            "arch": "x86_64",
            "virt": "kvm",
            "cpuCount": 4,
            "memoryTotalBytes": 8_000_000_000_u64,
            "disks": [{ "path": "/dev/sda", "sizeBytes": 40_000_000_000_u64, "role": "root" }],
            "docker": { "kind": "absent" },
            "cgroup": "v2",
            "zfs": { "kind": "eligible", "targets": [{ "kind": "main_disk" }] },
            "blockers": [],
            "warnings": [],
        });
        if let (Some(prep), Some(fields)) = (prep, report.as_object_mut()) {
            fields.insert("prep".into(), prep);
        }
        let raw = json!({
            "id": "6b0f3c1e-0000-4000-8000-000000000001",
            "name": "web-1",
            "status": status,
            "observed_ip": "203.0.113.9",
            "fleet_token": "amber-falcon",
            "report": report,
            "first_seen_at": "2026-10-16T00:00:00.000Z",
            "last_check_in_at": "2026-10-16T00:05:00.000Z",
        });
        Waiting {
            row: serde_json::from_value(raw.clone()).unwrap(),
            raw,
        }
    }

    #[test]
    fn the_status_cell_names_preparation_like_the_dashboard() {
        let readiness = |status: &str, prep: Option<Value>| waiting(status, prep).row.readiness().0;
        assert_eq!(readiness("offline", None), "Offline");
        assert_eq!(readiness("blocked", None), "Blocked");
        assert_eq!(
            readiness("blocked", Some(json!({ "docker": "failed", "zfs": "off" }))),
            "Docker install failed"
        );
        assert_eq!(
            readiness(
                "preparing",
                Some(json!({ "docker": "installing", "zfs": "off" }))
            ),
            "Installing Docker"
        );
        assert_eq!(
            readiness(
                "waiting",
                Some(json!({ "docker": "ready", "zfs": "installing" }))
            ),
            "Building ZFS"
        );
        assert_eq!(readiness("waiting", None), "Ready");
        assert_eq!(readiness("enrolling", None), "Unknown");
    }

    #[test]
    fn a_host_cannot_print_escape_sequences() {
        let raw = json!({ "name": "web\u{1b}[2J-1", "blockers": [{ "message": "a\u{7}b" }] });
        assert_eq!(
            printable(raw),
            json!({ "name": "web\u{FFFD}[2J-1", "blockers": [{ "message": "a\u{FFFD}b" }] })
        );
    }

    #[test]
    fn the_ip_is_where_cloud_saw_the_host() {
        let mut host = waiting("waiting", None);
        assert_eq!(host.row.from(), "203.0.113.9");
        host.row.report.public_ip = Some("203.0.113.9".into());
        assert_eq!(host.row.from(), "203.0.113.9");
        host.row.report.public_ip = Some("198.51.100.4".into());
        assert_eq!(host.row.ip(), "203.0.113.9");
        assert_eq!(host.row.from(), "203.0.113.9 (reports 198.51.100.4)");
    }

    #[test]
    fn storage_says_why_a_host_gets_docker_volumes() {
        let host = waiting("waiting", None);
        assert_eq!(host.row.storage(), "ZFS on the main disk");
        let failed = waiting(
            "waiting",
            Some(json!({ "docker": "ready", "zfs": "failed" })),
        );
        assert_eq!(failed.row.storage(), "Docker volumes; the ZFS build failed");
    }

    #[test]
    fn age_counts_in_the_largest_whole_unit() {
        let now = DateTime::parse_from_rfc3339("2026-10-16T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(age("2026-10-16T02:59:30Z", now), "30s");
        assert_eq!(age("2026-10-16T02:15:00Z", now), "45m");
        assert_eq!(age("2026-10-15T03:00:00Z", now), "24h");
        assert_eq!(age("2026-10-12T03:00:00Z", now), "4d");
        assert_eq!(age("2026-10-16T03:00:10Z", now), "0s");
    }
}
