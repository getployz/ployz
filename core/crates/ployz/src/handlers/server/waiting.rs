//! Servers waiting to join: hosts that checked in to Cloud with a fleet token and wait for
//! someone to enroll or reject them.

use chrono::{DateTime, Utc};
use clap::ArgMatches;
use clap::parser::ValueSource;
use ployz_core::RpcErrorCode;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::signed_in;
use crate::cloud_account::{self, Credential, StoreCallError};
use crate::cloud_login::LoginError;
use crate::handlers::{Error, runtime};
use crate::ui::{self, Cell, Fields, Hint, Table, Tone, printable};

/// One waiting server: the row Cloud sent, which `--json` prints, and the fields read from it.
/// Both keep Cloud's strings; only what a person reads goes through [`printable`].
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
    report: Summary,
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
struct Summary {
    provider: Option<String>,
    prep: Option<Prep>,
}

pub(super) struct Detail {
    waiting: Waiting,
    report: Report,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    public_ip: Option<String>,
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
            let row = serde_json::from_value(raw.clone())?;
            Ok(Waiting { raw, row })
        })
        .collect()
}

pub(super) async fn detail(credential: &Credential, id: &str) -> Result<Detail, Error> {
    #[derive(Deserialize)]
    struct Reply {
        waiting_server: Value,
    }
    let Reply { waiting_server } = cloud_account::refusable(
        credential,
        Method::GET,
        &format!("waiting-servers/{id}"),
        None,
    )
    .await?;
    Ok(Detail::read(waiting_server)?)
}

/// Cloud takes at most this many ids in one reject.
const REJECT_BATCH: usize = 1000;

/// What a reject did: Cloud's count over the batches it answered, and where it stopped.
pub(super) struct Rejection<'a> {
    pub(super) rejected: u64,
    pub(super) stopped: Option<Stopped<'a>>,
}

/// A reject cut short: the ids Cloud may have rejected, and those it surely did not,
/// because it refused them or they were never sent.
pub(super) struct Stopped<'a> {
    pub(super) error: Error,
    pub(super) uncertain: &'a [&'a str],
    pub(super) not_rejected: &'a [&'a str],
}

/// Reject `ids` a batch at a time. A first batch Cloud surely refused is the error itself.
pub(super) async fn reject<'a>(
    credential: &Credential,
    ids: &'a [&'a str],
) -> Result<Rejection<'a>, Error> {
    #[derive(Deserialize)]
    struct Reply {
        rejected: u64,
    }
    let mut rejected = 0;
    let mut rest = ids;
    while !rest.is_empty() {
        let (batch, after) = rest.split_at(rest.len().min(REJECT_BATCH));
        let body = json!({ "ids": batch });
        match cloud_account::refusable::<Reply>(
            credential,
            Method::POST,
            "reject-servers",
            Some(&body),
        )
        .await
        {
            Ok(reply) => rejected += reply.rejected,
            Err(error) => {
                let may_have_acted = may_have_acted(&error);
                let error = Error::from(error).context("Could not reject the waiting servers.");
                if !may_have_acted && rest.len() == ids.len() {
                    return Err(error);
                }
                let (uncertain, not_rejected) = match may_have_acted {
                    true => (batch, after),
                    false => (&[][..], rest),
                };
                return Ok(Rejection {
                    rejected,
                    stopped: Some(Stopped {
                        error,
                        uncertain,
                        not_rejected,
                    }),
                });
            }
        }
        rest = after;
    }
    Ok(Rejection {
        rejected,
        stopped: None,
    })
}

/// Whether Cloud may have acted on a call that failed: it timed out, dropped the
/// connection, failed inside, or refused with a code this build cannot read. Every
/// other refusal left the list as it was.
pub(super) fn may_have_acted(error: &StoreCallError) -> bool {
    match error {
        StoreCallError::Refused(refusal) => {
            matches!(
                refusal.code,
                RpcErrorCode::Internal | RpcErrorCode::Unavailable | RpcErrorCode::Unknown(_)
            )
        }
        StoreCallError::Cloud(LoginError::Status { status, .. }) => *status >= 500,
        StoreCallError::Cloud(LoginError::Unreachable { .. } | LoginError::Reply(_))
        | StoreCallError::Stopped(_) => true,
        StoreCallError::Cloud(
            LoginError::Unsupported(_)
            | LoginError::Store { .. }
            | LoginError::Corrupt { .. }
            | LoginError::OtherCloud { .. }
            | LoginError::SignedOut
            | LoginError::Expired
            | LoginError::Denied
            | LoginError::Ended
            | LoginError::AwaitingApproval { .. }
            | LoginError::TokenRefused
            | LoginError::NotMember(_)
            | LoginError::TokenBound
            | LoginError::UnknownOrganization(_)
            | LoginError::UnknownCredential(_),
        ) => false,
    }
}

const ID_PREFIX_MIN: usize = 8;

/// The waiting server whose id `selector` is, else those named `selector` exactly, else the
/// one whose id starts with it (eight characters or more), in the list's order.
pub(super) fn named<'a>(listed: &'a [Waiting], selector: &str) -> Result<Vec<&'a Waiting>, Error> {
    if let Some(host) = listed.iter().find(|host| host.row.id == selector) {
        return Ok(vec![host]);
    }
    let named: Vec<_> = listed
        .iter()
        .filter(|host| host.row.name == selector)
        .collect();
    if !named.is_empty() || selector.chars().count() < ID_PREFIX_MIN {
        return Ok(named);
    }
    let started: Vec<_> = listed
        .iter()
        .filter(|host| host.row.id.starts_with(selector))
        .collect();
    if started.len() > 1 {
        return Err(ambiguous(
            format!(
                "{} waiting servers have an id starting {}. Use more of the id",
                started.len(),
                printable(selector)
            ),
            &started,
        ));
    }
    Ok(started)
}

pub(super) fn ambiguous(sentence: String, hosts: &[&Waiting]) -> Error {
    let ids = hosts
        .iter()
        .map(|host| format!("{} ({})", printable(&host.row.id), printable(host.row.ip())))
        .collect::<Vec<_>>()
        .join(", ");
    Error::detailed(
        RpcErrorCode::Ambiguous,
        format!("{sentence}: {ids}"),
        json!({
            "waiting_servers": hosts
                .iter()
                .map(|host| json!({ "id": host.row.id, "ip": host.row.ip() }))
                .collect::<Vec<_>>(),
        }),
    )
}

pub(super) fn copied_hint(selector: &str) -> Option<Hint> {
    selector
        .contains(ui::REPLACED)
        .then(|| Hint::Inspect("ployz server ls --waiting --json".into()))
}

/// Whether `--context` or `--connect` was typed on the command line. `PLOYZ_CONTEXT` and
/// `PLOYZ_CONNECT` in the environment don't count: the waiting list lives in Cloud alone.
pub(super) fn points_elsewhere(matches: &ArgMatches) -> bool {
    ["connect", "context"]
        .into_iter()
        .any(|id| matches.value_source(id) == Some(ValueSource::CommandLine))
}

pub(super) fn inherits_a_connection(matches: &ArgMatches) -> bool {
    ["connect", "context"]
        .into_iter()
        .any(|id| matches.value_source(id) == Some(ValueSource::EnvVariable))
}

/// The credential a waiting-list command runs under. `--context` and `--connect` are
/// refused, and signing out asks for `ployz login`.
pub(super) fn credential(
    runtime: &tokio::runtime::Runtime,
    matches: &ArgMatches,
) -> Result<Credential, Error> {
    if points_elsewhere(matches) {
        return Err(Error::usage(
            "servers waiting to join live in Ployz Cloud; drop --context and --connect",
        ));
    }
    signed_in(runtime, matches)?.ok_or_else(|| LoginError::SignedOut.into())
}

/// `server ls --waiting`: one fixed row per waiting server.
pub(super) fn list(matches: &ArgMatches) -> Result<(), Error> {
    let runtime = runtime()?;
    let credential = credential(&runtime, matches)?;
    let waiting = runtime.block_on(fetch(&credential))?;
    let now = Utc::now();
    let mut table = Table::new(
        ["ID", "NAME", "PROVIDER", "IP", "STATUS", "AGE"],
        "No servers waiting.",
    );
    for Waiting { row, .. } in &waiting {
        let (status, tone) = row.readiness();
        table.row([
            Cell::from(printable(ui::short_id(&row.id))),
            Cell::from(printable(&row.name)),
            Cell::from(printable(row.provider())),
            Cell::from(printable(row.ip())),
            Cell::status(status, tone),
            Cell::from(age(&row.first_seen_at, now)),
        ]);
    }
    let raw: Vec<&Value> = waiting.iter().map(|waiting| &waiting.raw).collect();
    ui::list(&json!({ "waiting_servers": raw }), &table)
}

/// `server show` for a waiting server: its facts, then what keeps it from joining.
pub(super) fn show(detail: &Detail) -> Result<(), Error> {
    let Detail { waiting, report } = detail;
    let row = &waiting.row;
    let now = Utc::now();
    let (status, _) = row.readiness();
    let mut lines = vec![
        ("waiting server", row.name.clone()),
        ("status", status.to_owned()),
        (
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
        ),
        ("disks", detail.disks()),
        ("docker", detail.docker()),
        ("storage", detail.storage()),
        (
            "from",
            format!(
                "{} · {} · {}",
                detail.addresses(),
                row.provider(),
                row.fleet_token
            ),
        ),
        (
            "first seen",
            format!("{} ago", age(&row.first_seen_at, now)),
        ),
        (
            "last check-in",
            format!("{} ago", age(&row.last_check_in_at, now)),
        ),
        ("id", row.id.clone()),
    ];
    lines.extend(
        report
            .blockers
            .iter()
            .map(|blocker| ("blocked", blocker.message.clone())),
    );
    lines.extend(
        report
            .warnings
            .iter()
            .map(|warning| ("warning", warning.message.clone())),
    );
    let mut record = Fields::new();
    for (label, value) in lines {
        record.push(label, printable(&value));
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
}

impl Detail {
    fn read(raw: Value) -> Result<Self, serde_json::Error> {
        #[derive(Deserialize)]
        struct Full {
            report: Report,
        }
        let row = serde_json::from_value(raw.clone())?;
        let Full { report } = serde_json::from_value(raw.clone())?;
        Ok(Self {
            waiting: Waiting { raw, row },
            report,
        })
    }

    fn addresses(&self) -> String {
        let observed = &self.waiting.row.observed_ip;
        match self.report.public_ip.as_deref() {
            Some(reported) if reported != observed => format!("{observed} (reports {reported})"),
            _ => observed.clone(),
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
            .waiting
            .row
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

    const FIXTURE: &str = include_str!("../../../tests/fixtures/waiting-servers.json");

    fn served(pointer: &str) -> Value {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        fixture.pointer(pointer).unwrap().clone()
    }

    pub(crate) fn waiting(status: &str, prep: Option<Value>) -> Waiting {
        let mut raw = served("/list/waiting_servers/0");
        *raw.get_mut("status").unwrap() = json!(status);
        let report = raw.pointer_mut("/report").unwrap().as_object_mut().unwrap();
        match prep {
            Some(prep) => report.insert("prep".into(), prep),
            None => report.remove("prep"),
        };
        Waiting {
            row: serde_json::from_value(raw.clone()).unwrap(),
            raw,
        }
    }

    fn shown(prep: Option<Value>) -> Detail {
        let mut raw = served("/detail/waiting_server");
        let report = raw.pointer_mut("/report").unwrap().as_object_mut().unwrap();
        match prep {
            Some(prep) => report.insert("prep".into(), prep),
            None => report.remove("prep"),
        };
        Detail::read(raw).unwrap()
    }

    #[test]
    fn the_list_and_the_detail_read_what_cloud_serves() {
        let listed = waiting("waiting", None);
        assert_eq!(listed.row.name, "web-1");
        assert_eq!(listed.row.provider(), "hetzner");
        let detail = shown(None);
        assert_eq!(detail.disks(), "/dev/sda 40 GB root, /dev/sdb 100 GB empty");
        assert_eq!(detail.docker(), "27.3.1");
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
    fn the_ip_is_where_cloud_saw_the_host() {
        let mut host = shown(None);
        assert_eq!(host.waiting.row.ip(), "203.0.113.9");
        assert_eq!(host.addresses(), "203.0.113.9 (reports 198.51.100.4)");
        host.report.public_ip = Some("203.0.113.9".into());
        assert_eq!(host.addresses(), "203.0.113.9");
        host.report.public_ip = None;
        assert_eq!(host.addresses(), "203.0.113.9");
    }

    #[test]
    fn storage_says_why_a_host_gets_docker_volumes() {
        assert_eq!(
            shown(None).storage(),
            "ZFS on the main disk or /dev/disk/by-id/scsi-0HC_Volume_1"
        );
        let failed = shown(Some(json!({ "docker": "ready", "zfs": "failed" })));
        assert_eq!(failed.storage(), "Docker volumes; the ZFS build failed");
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
