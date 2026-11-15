//! Servers waiting to join: hosts that checked in to Cloud with a fleet token and wait for
//! someone to enroll or reject them.

use chrono::{DateTime, Utc};
use clap::ArgMatches;
use clap::parser::ValueSource;
use ployz_core::RpcErrorCode;
use reqwest::Method;
use serde::de::IntoDeserializer;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::super::route::signed_in;
use crate::checkin::{
    DOCKER_INSTALL_FAILED, DockerPrep, HostReport, Prep, ReportDocker, ReportTarget, ReportZfs,
    ZfsPrep,
};
use crate::cloud_account::{self, Credential, StoreCallError};
use crate::cloud_login::LoginError;
use crate::handlers::{Error, runtime};
use crate::ui::{self, Cell, Fields, Table, Tone, printable};
use ployz_core::Hint;

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
    #[serde(default, deserialize_with = "prep_states")]
    prep: Option<Prep>,
    /// The host's blockers; a list row carries only the first.
    #[serde(default)]
    blockers: Vec<Blocker>,
}

#[derive(Deserialize)]
struct Blocker {
    code: String,
}

/// Reads `prep` as the two strings Cloud serves, so a state this release does not know is
/// [`DockerPrep::Unknown`] or [`ZfsPrep::Unknown`] and a state of another type is refused as a string.
fn prep_states<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Prep>, D::Error> {
    #[derive(Deserialize)]
    struct Prep {
        docker: String,
        zfs: String,
    }
    Option::<Prep>::deserialize(deserializer)?
        .map(|Prep { docker, zfs }| {
            Ok(crate::checkin::Prep {
                docker: DockerPrep::deserialize(docker.into_deserializer())?,
                zfs: ZfsPrep::deserialize(zfs.into_deserializer())?,
            })
        })
        .transpose()
}

pub(super) struct Detail {
    waiting: Waiting,
    report: HostReport,
    public_ip: Option<String>,
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

/// An open enroll attempt. Its host has left the waiting list, and rejecting the host by
/// `waiting_server_id` cancels the attempt, unless Cloud sent its join.
#[derive(Deserialize)]
pub(super) struct Enrolling {
    pub(super) id: String,
    pub(super) waiting_server_id: String,
    pub(super) name: String,
    state: String,
    stage: Option<String>,
}

impl Enrolling {
    /// Whether Cloud sent the join, so the Server may already be in the Cluster.
    pub(super) fn joining(&self) -> bool {
        matches!(self.stage.as_deref(), Some("joining" | "publishing"))
    }
}

pub(super) async fn enrolling(credential: &Credential) -> Result<Vec<Enrolling>, Error> {
    #[derive(Deserialize)]
    struct Reply {
        enrollments: Vec<Enrolling>,
    }
    let Reply { enrollments } =
        cloud_account::refusable(credential, Method::GET, "enrollments", None).await?;
    Ok(enrollments
        .into_iter()
        .filter(|attempt| !matches!(attempt.state.as_str(), "joined" | "failed" | "cancelled"))
        .collect())
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
    /// The waiting server ids Cloud rejected but whose open attempt it left running,
    /// because it may have sent the join.
    pub(super) spared: Vec<String>,
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
        #[serde(default)]
        spared: Vec<String>,
    }
    let mut rejected = 0;
    let mut spared = Vec::new();
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
            Ok(reply) => {
                rejected += reply.rejected;
                spared.extend(reply.spared);
            }
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
                    spared,
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
        spared,
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
        StoreCallError::Cloud(
            LoginError::Unreachable { .. } | LoginError::Unanswered { .. } | LoginError::Reply(_),
        )
        | StoreCallError::Stopped(_) => true,
        StoreCallError::Cloud(
            LoginError::Unsupported(_)
            | LoginError::Store { .. }
            | LoginError::Corrupt { .. }
            | LoginError::TokenCloudUnreadable(_)
            | LoginError::OtherCloud { .. }
            | LoginError::SignedOut
            | LoginError::Expired
            | LoginError::Denied
            | LoginError::Ended
            | LoginError::AwaitingApproval { .. }
            | LoginError::TokenRefused
            | LoginError::AgentTokenRefused
            | LoginError::TokenMalformed
            | LoginError::NotMember(_)
            | LoginError::TokenBound
            | LoginError::UnknownOrganization(_)
            | LoginError::UnknownCredential(_)
            | LoginError::AmbiguousCredential { .. },
        ) => false,
    }
}

pub(super) const ID_PREFIX_MIN: usize = 8;

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
    let Detail {
        waiting, report, ..
    } = detail;
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
        // The daemon lists a host's own blockers before docker_install_failed, so one of those reads Blocked.
        let docker_failed = prep.is_some_and(|prep| prep.docker == DockerPrep::Failed)
            && self
                .report
                .blockers
                .first()
                .is_none_or(|blocker| blocker.code == DOCKER_INSTALL_FAILED);
        match self.status {
            Status::Offline => ("Offline", Tone::Bad),
            Status::Blocked | Status::Preparing if docker_failed => {
                ("Docker install failed", Tone::Bad)
            }
            Status::Blocked => ("Blocked", Tone::Bad),
            Status::Preparing => ("Installing Docker", Tone::Change),
            Status::Waiting if prep.is_some_and(|prep| prep.zfs == ZfsPrep::Installing) => {
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
            report: HostReport,
        }
        #[derive(Deserialize)]
        struct Served {
            report: Added,
        }
        #[derive(Deserialize)]
        struct Added {
            #[serde(rename = "publicIp")]
            public_ip: Option<String>,
        }
        let row = serde_json::from_value(raw.clone())?;
        let Full { report } = serde_json::from_value(raw.clone())?;
        let Served {
            report: Added { public_ip },
        } = serde_json::from_value(raw.clone())?;
        Ok(Self {
            waiting: Waiting { raw, row },
            report,
            public_ip,
        })
    }

    fn addresses(&self) -> String {
        let observed = &self.waiting.row.observed_ip;
        match self.public_ip.as_deref() {
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
            ReportDocker::Present { version, .. } => version.clone(),
            ReportDocker::Absent => "not installed".into(),
            ReportDocker::Unknown => "unknown".into(),
        }
    }

    fn storage(&self) -> String {
        if self
            .waiting
            .row
            .report
            .prep
            .as_ref()
            .is_some_and(|prep| prep.zfs == ZfsPrep::Failed)
        {
            return "Docker volumes; the ZFS build failed".into();
        }
        match &self.report.zfs {
            ReportZfs::Eligible { targets, .. } => {
                let on = targets
                    .iter()
                    .map(|target| match target {
                        ReportTarget::MainDisk => "the main disk".to_owned(),
                        ReportTarget::Disk { by_id } => by_id.clone(),
                        ReportTarget::Unknown => "another disk".to_owned(),
                    })
                    .collect::<Vec<_>>();
                format!("ZFS on {}", on.join(" or "))
            }
            ReportZfs::Ineligible { message, .. } => format!("Docker volumes; {message}"),
            ReportZfs::Unknown => "unknown".into(),
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
    fn a_hosts_own_blocker_reads_blocked_over_the_docker_install_it_stopped() {
        // Cloud lists a row's first blocker; the daemon puts the host's own before docker_install_failed.
        let docker_failed = |status: &str, blockers: Option<Value>| {
            let mut host = waiting(status, Some(json!({ "docker": "failed", "zfs": "off" })));
            let report = host
                .raw
                .pointer_mut("/report")
                .unwrap()
                .as_object_mut()
                .unwrap();
            match blockers {
                Some(blockers) => report.insert("blockers".into(), blockers),
                None => report.remove("blockers"),
            };
            serde_json::from_value::<Row>(host.raw)
                .unwrap()
                .readiness()
                .0
        };
        let disk =
            json!({ "code": "disk_too_small", "message": "Disk is 24 GiB. Ployz needs 30 GiB." });
        let docker = json!({ "code": "docker_install_failed", "message": "apt-get exited 100." });
        // A list row carries the first blocker, `server show` all of them.
        assert_eq!(docker_failed("blocked", Some(json!([disk]))), "Blocked");
        assert_eq!(
            docker_failed("blocked", Some(json!([disk, docker]))),
            "Blocked"
        );
        assert_eq!(
            docker_failed("blocked", Some(json!([docker]))),
            "Docker install failed"
        );
        // Cloud reads a host with no blocker as preparing; an older Cloud sends no blockers at all.
        assert_eq!(
            docker_failed("preparing", Some(json!([]))),
            "Docker install failed"
        );
        assert_eq!(docker_failed("preparing", None), "Docker install failed");
    }

    #[test]
    fn the_ip_is_where_cloud_saw_the_host() {
        let mut host = shown(None);
        assert_eq!(host.waiting.row.ip(), "203.0.113.9");
        assert_eq!(host.addresses(), "203.0.113.9 (reports 198.51.100.4)");
        host.public_ip = Some("203.0.113.9".into());
        assert_eq!(host.addresses(), "203.0.113.9");
        host.public_ip = None;
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
    fn a_kind_a_newer_cloud_adds_still_reads_and_shows_as_unknown() {
        let mut raw = served("/detail/waiting_server");
        *raw.pointer_mut("/report/docker/kind").unwrap() = json!("podman");
        *raw.pointer_mut("/report/zfs").unwrap() = json!({ "kind": "btrfs" });
        *raw.pointer_mut("/report/disks/1/role").unwrap() = json!("spare");
        *raw.pointer_mut("/report/prep").unwrap() = json!({ "docker": "paused", "zfs": "later" });
        let detail = Detail::read(raw.clone()).unwrap();
        assert_eq!(detail.docker(), "unknown");
        assert_eq!(detail.storage(), "unknown");
        assert_eq!(detail.disks(), "/dev/sda 40 GB root, /dev/sdb 100 GB spare");
        assert_eq!(detail.waiting.row.readiness().0, "Offline");
        *raw.pointer_mut("/status").unwrap() = json!("waiting");
        assert_eq!(
            Detail::read(raw).unwrap().waiting.row.readiness().0,
            "Ready"
        );
        let mut raw = served("/detail/waiting_server");
        raw.pointer_mut("/report/zfs/targets")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({ "kind": "tape" }));
        assert_eq!(
            Detail::read(raw).unwrap().storage(),
            "ZFS on the main disk or /dev/disk/by-id/scsi-0HC_Volume_1 or another disk"
        );
    }

    /// Sets the value a pointer names, adding the key when it is missing; `None` removes it.
    fn edit(raw: &mut Value, pointer: &str, value: Option<Value>) {
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        let parent = raw.pointer_mut(parent).unwrap().as_object_mut().unwrap();
        match value {
            Some(value) => parent.insert(key.into(), value),
            None => parent.remove(key),
        };
    }

    /// What `server show` prints from the report that these rows could change.
    fn displayed(detail: &Detail) -> [String; 4] {
        [
            detail.docker(),
            detail.storage(),
            detail.disks(),
            detail.report.cpu_count.to_string(),
        ]
    }

    fn refusal<T>(read: Result<T, serde_json::Error>) -> Option<String> {
        read.err().map(|error| error.to_string())
    }

    fn ineligible(extra: Option<(&'static str, Value)>) -> Vec<(&'static str, Option<Value>)> {
        let mut edits = vec![
            ("/report/zfs/kind", Some(json!("ineligible"))),
            ("/report/zfs/targets", None),
            ("/report/zfs/buildMinutes", None),
            ("/report/zfs/message", Some(json!("no ZFS"))),
        ];
        edits.extend(extra.map(|(pointer, value)| (pointer, Some(value))));
        edits
    }

    /// Each row changes one part of a served report. A row that `server show` read before this
    /// release still reads, to the same lines and the same raw JSON.
    #[test]
    fn a_served_row_reads_as_far_as_what_it_shows_goes() {
        let baseline = Detail::read(served("/detail/waiting_server")).unwrap();
        let unchanged = displayed(&baseline);
        let mut no_zfs = unchanged.clone();
        no_zfs[1] = "Docker volumes; no ZFS".into();
        let mut big_cpu = unchanged.clone();
        big_cpu[3] = "4294967296".into();
        let gone = |pointer| vec![(pointer, None)];
        let set = |pointer, value| vec![(pointer, Some(value))];
        let rows = [
            ("hostname absent", gone("/report/hostname"), &unchanged),
            (
                "hostname null",
                set("/report/hostname", json!(null)),
                &unchanged,
            ),
            (
                "hostname wrong",
                set("/report/hostname", json!(32)),
                &unchanged,
            ),
            ("kernel absent", gone("/report/kernel"), &unchanged),
            (
                "kernel null",
                set("/report/kernel", json!(null)),
                &unchanged,
            ),
            ("kernel wrong", set("/report/kernel", json!(32)), &unchanged),
            ("cgroup absent", gone("/report/cgroup"), &unchanged),
            (
                "cgroup null",
                set("/report/cgroup", json!(null)),
                &unchanged,
            ),
            (
                "cgroup wrong",
                set("/report/cgroup", json!({ "kind": "v2" })),
                &unchanged,
            ),
            (
                "cgroup unknown",
                set("/report/cgroup", json!("v3")),
                &unchanged,
            ),
            ("docker api absent", gone("/report/docker/api"), &unchanged),
            (
                "docker api null",
                set("/report/docker/api", json!(null)),
                &unchanged,
            ),
            (
                "docker api wrong",
                set("/report/docker/api", json!(7)),
                &unchanged,
            ),
            (
                "docker compatible absent",
                gone("/report/docker/compatible"),
                &unchanged,
            ),
            (
                "docker compatible null",
                set("/report/docker/compatible", json!(null)),
                &unchanged,
            ),
            (
                "docker compatible wrong",
                set("/report/docker/compatible", json!([])),
                &unchanged,
            ),
            (
                "buildMinutes null",
                set("/report/zfs/buildMinutes", json!(null)),
                &unchanged,
            ),
            (
                "buildMinutes wrong",
                set("/report/zfs/buildMinutes", json!([])),
                &unchanged,
            ),
            (
                "buildMinutes fraction",
                set("/report/zfs/buildMinutes", json!(6.5)),
                &unchanged,
            ),
            (
                "buildMinutes negative",
                set("/report/zfs/buildMinutes", json!(-1)),
                &unchanged,
            ),
            (
                "buildMinutes past u32",
                set("/report/zfs/buildMinutes", json!(4_294_967_296_u64)),
                &unchanged,
            ),
            (
                "buildMinutes absent",
                gone("/report/zfs/buildMinutes"),
                &unchanged,
            ),
            (
                "byId null",
                set("/report/disks/1/byId", json!(null)),
                &unchanged,
            ),
            (
                "byId wrong",
                set("/report/disks/1/byId", json!([])),
                &unchanged,
            ),
            (
                "availableBytes absent",
                gone("/report/disks/0/availableBytes"),
                &unchanged,
            ),
            (
                "availableBytes null",
                set("/report/disks/0/availableBytes", json!(null)),
                &unchanged,
            ),
            (
                "availableBytes wrong",
                set("/report/disks/0/availableBytes", json!([])),
                &unchanged,
            ),
            (
                "warning code absent",
                gone("/report/warnings/0/code"),
                &unchanged,
            ),
            (
                "warning code null",
                set("/report/warnings/0/code", json!(null)),
                &unchanged,
            ),
            (
                "warning code wrong",
                set("/report/warnings/0/code", json!([])),
                &unchanged,
            ),
            (
                "cpuCount past u32",
                set("/report/cpuCount", json!(4_294_967_296_u64)),
                &big_cpu,
            ),
            ("ineligible, a message only", ineligible(None), &no_zfs),
            (
                "ineligible, with a code",
                ineligible(Some(("/report/zfs/code", json!("no_zfs")))),
                &no_zfs,
            ),
            (
                "a field added later",
                set("/report/future", json!({ "any": [1, 2, 3] })),
                &unchanged,
            ),
        ];
        for (name, edits, shows) in rows {
            let mut raw = served("/detail/waiting_server");
            for (pointer, value) in edits {
                edit(&mut raw, pointer, value);
            }
            let detail =
                Detail::read(raw.clone()).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(&displayed(&detail), shows, "{name}");
            assert_eq!(detail.waiting.raw, raw, "{name}");
            assert_eq!(refusal(serde_json::from_value::<Row>(raw)), None, "{name}");
        }
    }

    /// The waiting list reads a row's blockers by their code and its prep by two strings;
    /// `server show` also reads each blocker's message from the full report.
    #[test]
    fn the_list_reads_a_blocker_by_its_code_and_prep_as_two_strings() {
        let code_absent = "missing field `code`";
        let null = "invalid type: null, expected a string";
        let integer = "invalid type: integer `123`, expected a string";
        let message_absent = "missing field `message`";
        let set = |pointer, value| vec![(pointer, Some(value))];
        let gone = |pointer| vec![(pointer, None)];
        let rows = [
            (
                "code absent",
                gone("/report/blockers/0/code"),
                Some(code_absent),
                Some(code_absent),
            ),
            (
                "code null",
                set("/report/blockers/0/code", json!(null)),
                Some(null),
                Some(null),
            ),
            (
                "code wrong",
                set("/report/blockers/0/code", json!(123)),
                Some(integer),
                Some(integer),
            ),
            (
                "message absent",
                gone("/report/blockers/0/message"),
                None,
                Some(message_absent),
            ),
            (
                "message null",
                set("/report/blockers/0/message", json!(null)),
                None,
                Some(null),
            ),
            (
                "message wrong",
                set("/report/blockers/0/message", json!(123)),
                None,
                Some(integer),
            ),
            ("prep null", set("/report/prep", json!(null)), None, None),
            (
                "prep wrong",
                set("/report/prep", json!(123)),
                Some("invalid type: integer `123`, expected struct Prep"),
                Some("invalid type: integer `123`, expected struct Prep"),
            ),
            (
                "prep docker absent",
                gone("/report/prep/docker"),
                Some("missing field `docker`"),
                Some("missing field `docker`"),
            ),
            (
                "prep zfs wrong",
                set("/report/prep/zfs", json!(123)),
                Some(integer),
                Some(integer),
            ),
        ];
        for (name, edits, list, show) in rows {
            let mut raw = served("/detail/waiting_server");
            for (pointer, value) in edits {
                edit(&mut raw, pointer, value);
            }
            let listed = serde_json::from_value::<Row>(raw.clone());
            assert_eq!(refusal(listed).as_deref(), list, "{name}: list");
            if list.is_none() {
                let row = serde_json::from_value::<Row>(raw.clone()).unwrap();
                assert_eq!(row.readiness().0, "Offline", "{name}");
            }
            assert_eq!(refusal(Detail::read(raw)).as_deref(), show, "{name}: show");
        }
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
