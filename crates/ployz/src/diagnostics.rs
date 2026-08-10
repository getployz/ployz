//! Remote status and doctor execution with human-first presentation.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use hyper::Method;
use ployz_core::{
    DOCTOR_ROUTE, DoctorDocument, DoctorForeignAuthorship, DoctorMalformedRosterDocumentClass,
    DoctorRosterRowSkipReason, DoctorRosterTable, HandshakeFreshness, STATUS_ROUTE,
    StatusAnsweringMachine, StatusBarrier, StatusDegradationReason, StatusDocument,
    StatusHandshakeEvidence, StatusHint, StatusSync,
};

use crate::commands::DiagnosticsCommand;
use crate::init::ssh::shell_quote;
use crate::remote::{OperatorRemote, OperatorRemoteError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticsOutcomeKind {
    Report,
    Findings,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsOutcome {
    kind: DiagnosticsOutcomeKind,
    output: String,
}

impl DiagnosticsOutcome {
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self.kind, DiagnosticsOutcomeKind::Report)
    }

    #[must_use]
    pub const fn is_unreachable(&self) -> bool {
        matches!(self.kind, DiagnosticsOutcomeKind::Unreachable)
    }

    #[must_use]
    pub fn output(&self) -> &str {
        &self.output
    }

    fn report(output: String) -> Self {
        Self {
            kind: DiagnosticsOutcomeKind::Report,
            output,
        }
    }

    fn findings(output: String) -> Self {
        Self {
            kind: DiagnosticsOutcomeKind::Findings,
            output,
        }
    }

    fn unreachable(error: &OperatorRemoteError) -> Self {
        Self {
            kind: DiagnosticsOutcomeKind::Unreachable,
            output: render_unreachable(error),
        }
    }
}

pub async fn status(command: DiagnosticsCommand) -> DiagnosticsOutcome {
    let remote = match OperatorRemote::load(command.target.as_ref()) {
        Ok(remote) => remote,
        Err(error) => return DiagnosticsOutcome::unreachable(&error),
    };
    match remote
        .request_json::<(), StatusDocument>(Method::GET, STATUS_ROUTE, None)
        .await
    {
        Ok(document) => DiagnosticsOutcome::report(render_status(&document)),
        Err(error) => DiagnosticsOutcome::unreachable(&error),
    }
}

pub async fn doctor(command: DiagnosticsCommand) -> DiagnosticsOutcome {
    let remote = match OperatorRemote::load(command.target.as_ref()) {
        Ok(remote) => remote,
        Err(error) => return DiagnosticsOutcome::unreachable(&error),
    };
    match remote
        .request_json::<(), DoctorDocument>(Method::GET, DOCTOR_ROUTE, None)
        .await
    {
        Ok(document) => {
            let (output, has_findings) = render_doctor(&document);
            if has_findings {
                DiagnosticsOutcome::findings(output)
            } else {
                DiagnosticsOutcome::report(output)
            }
        }
        Err(error) => DiagnosticsOutcome::unreachable(&error),
    }
}

#[must_use]
pub fn render_status(document: &StatusDocument) -> String {
    let mut output = String::new();
    match &document.cluster {
        Some(cluster) => {
            let noun = if cluster.machine_count == 1 {
                "machine"
            } else {
                "machines"
            };
            writeln!(
                output,
                "cluster\t{}\t{}\t{} {noun}",
                cluster.name,
                cluster.id.as_str(),
                cluster.machine_count
            )
            .expect("writing to a String cannot fail");
        }
        None => output.push_str("cluster\tunknown\n"),
    }

    let answering_machine = match &document.answering_machine {
        StatusAnsweringMachine::Known { name, .. } => name.as_str(),
        StatusAnsweringMachine::Unknown { id } => id.as_str(),
    };
    match &document.sync {
        StatusSync::CaughtUp { p99_lag } => writeln!(
            output,
            "sync\tcaught up\tlag p99 {p99_lag}\t(as seen from {answering_machine})"
        ),
        StatusSync::Syncing {
            gaps,
            queue_size,
            p99_lag,
        } => writeln!(
            output,
            "sync\tsyncing\tgaps {gaps}, queue {queue_size}, lag p99 {p99_lag}\t(as seen from {answering_machine})"
        ),
        StatusSync::NoLagSample => writeln!(
            output,
            "sync\tno lag sample\t(as seen from {answering_machine})"
        ),
        StatusSync::Degraded { reason } => writeln!(
            output,
            "sync\tdegraded\t{}\t(as seen from {answering_machine})",
            render_degradation(*reason)
        ),
    }
    .expect("writing to a String cannot fail");

    let barrier = match document.barrier {
        StatusBarrier::Ready => "ready",
        StatusBarrier::CatchingUp => "catching up",
        StatusBarrier::NoRoster => "no roster",
    };
    writeln!(output, "barrier\t{barrier}").expect("writing to a String cannot fail");
    output.push('\n');
    // The row contract has no machine role, so the table contains only durable row facts.
    output.push_str("NAME\tHANDSHAKE\tADDR\n");
    for machine in &document.machines {
        writeln!(
            output,
            "{}\t{}\t{}",
            machine.name.as_str(),
            render_handshake(&machine.handshake),
            machine.address
        )
        .expect("writing to a String cannot fail");
    }

    match document.barrier {
        StatusBarrier::Ready => {}
        StatusBarrier::CatchingUp => output.push_str(
            "\nHint: the durable roster is still catching up; wait, then run `ployz doctor`.\n",
        ),
        StatusBarrier::NoRoster => output.push_str(
            "\nRepair: this machine has no durable roster; run `ployz machine join <token>`.\n",
        ),
    }
    if document.hints.contains(&StatusHint::AllPeerHandshakesStale) {
        output.push_str(
            "\nHint: every peer handshake is stale; this machine may be partitioned. Run `ployz network status`, then `ployz doctor`.\n",
        );
    }
    if matches!(document.sync, StatusSync::Degraded { .. }) {
        output.push_str("\nHint: run `ployz doctor` for deeper diagnostics.\n");
    }
    output
}

const fn render_degradation(reason: StatusDegradationReason) -> &'static str {
    match reason {
        StatusDegradationReason::CorrosionUnavailable => "Corrosion unavailable",
        StatusDegradationReason::InvalidCorrosionHealthResponse => {
            "invalid Corrosion health response"
        }
    }
}

fn render_handshake(handshake: &StatusHandshakeEvidence) -> String {
    match handshake {
        StatusHandshakeEvidence::SelfMachine => "self".to_owned(),
        StatusHandshakeEvidence::NoTestimony => "no testimony".to_owned(),
        StatusHandshakeEvidence::Never => "never".to_owned(),
        StatusHandshakeEvidence::Ago { seconds, freshness } => {
            let age = format!("{seconds}s ago");
            match freshness {
                HandshakeFreshness::Healthy => age,
                HandshakeFreshness::Stale => format!("{age} ⚠"),
            }
        }
    }
}

#[must_use]
pub fn render_doctor(document: &DoctorDocument) -> (String, bool) {
    let actionable_foreign_authors = actionable_foreign_authors(document);
    let has_findings = !document.skipped_roster_rows.is_empty()
        || !document.skipped_newer_versions.is_empty()
        || !document.versions.behind.is_empty()
        || !document.versions.invalid.is_empty()
        || !document.foreign_clusters.is_empty();
    let mut output = if has_findings {
        String::from("doctor\tfindings\n")
    } else {
        String::from("doctor\tclean — no actionable anomalies found\n")
    };

    for skipped in &document.skipped_roster_rows {
        writeln!(
            output,
            "\n⚠ skipped roster row: {} {} ({})",
            match skipped.table {
                DoctorRosterTable::Machines => "machine",
                DoctorRosterTable::Peers => "peer",
            },
            skipped.key,
            render_roster_skip_reason(&skipped.reason)
        )
        .expect("writing to a String cannot fail");
    }

    for skipped in &document.skipped_newer_versions {
        writeln!(
            output,
            "\n⚠ newer row version: {} {} has v={} (this binary supports v={})",
            skipped.table.as_str(),
            skipped.key,
            skipped.found,
            skipped.supported
        )
        .expect("writing to a String cannot fail");
    }

    if !document.versions.behind.is_empty() || !document.versions.invalid.is_empty() {
        output.push_str("\n⚠ mixed binary versions in the cluster\n");
        if let Some(newest) = &document.versions.newest {
            let names = newest
                .machines
                .iter()
                .map(|machine| machine.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(output, "    newest:  {} ({names})", newest.version)
                .expect("writing to a String cannot fail");
        }
        for behind in &document.versions.behind {
            writeln!(
                output,
                "    behind:  {} ({})",
                behind.version,
                behind.machine.name.as_str()
            )
            .expect("writing to a String cannot fail");
        }
        for invalid in &document.versions.invalid {
            writeln!(
                output,
                "    invalid:  {:?} ({})",
                invalid.version,
                invalid.machine.name.as_str()
            )
            .expect("writing to a String cannot fail");
        }
        let mut names = document
            .versions
            .behind
            .iter()
            .map(|behind| behind.machine.name.as_str())
            .chain(
                document
                    .versions
                    .invalid
                    .iter()
                    .map(|invalid| invalid.machine.name.as_str()),
            )
            .collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        let names = names
            .into_iter()
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(
            output,
            "  roll the lagging machines forward:  ployz machine upgrade -- {names}"
        )
        .expect("writing to a String cannot fail");
    }

    for foreign in &document.foreign_clusters {
        writeln!(
            output,
            "\nnote: {} foreign-cluster row(s) carry cluster_id {} and are ignored by readers",
            foreign.rows.len(),
            foreign.cluster_id
        )
        .expect("writing to a String cannot fail");
        for row in &foreign.rows {
            writeln!(
                output,
                "    {} {}: {}",
                row.table.as_str(),
                row.key,
                render_foreign_authorship(&row.authorship)
            )
            .expect("writing to a String cannot fail");
        }
    }
    for machine in actionable_foreign_authors.values() {
        writeln!(
            output,
            "\n⚠ {} is writing another cluster's id; repair it in this order:",
            machine.name.as_str()
        )
        .expect("writing to a String cannot fail");
        writeln!(
            output,
            "    ployz machine rm -- {}",
            shell_quote(machine.name.as_str())
        )
        .expect("writing to a String cannot fail");
        writeln!(
            output,
            "    on {}:  sudo ployz machine reset",
            machine.name.as_str()
        )
        .expect("writing to a String cannot fail");
        output.push_str("    ployz token create <name>  →  paste the join line on that machine\n");
    }

    (output, has_findings)
}

fn render_roster_skip_reason(reason: &DoctorRosterRowSkipReason) -> String {
    match reason {
        DoctorRosterRowSkipReason::MeshProviderMismatch { .. } => {
            "mesh provider mismatch".to_owned()
        }
        DoctorRosterRowSkipReason::MalformedDocument { class } => match class {
            DoctorMalformedRosterDocumentClass::MissingVersion => {
                "missing document version".to_owned()
            }
            DoctorMalformedRosterDocumentClass::InvalidVersion => {
                "invalid document version".to_owned()
            }
            DoctorMalformedRosterDocumentClass::UnsupportedVersion { .. } => {
                "unsupported document version".to_owned()
            }
            DoctorMalformedRosterDocumentClass::InvalidPayload => {
                "invalid document payload".to_owned()
            }
        },
        DoctorRosterRowSkipReason::InvalidRowKey { expected } => {
            format!("noncanonical row key; expected {expected}")
        }
    }
}

fn actionable_foreign_authors(
    document: &DoctorDocument,
) -> BTreeMap<String, ployz_core::DoctorMachineIdentity> {
    let mut machines = BTreeMap::new();
    for foreign in &document.foreign_clusters {
        for row in &foreign.rows {
            if let DoctorForeignAuthorship::CurrentMachine { machine } = &row.authorship {
                machines
                    .entry(machine.id.as_str().to_owned())
                    .or_insert_with(|| machine.clone());
            }
        }
    }
    machines
}

fn render_foreign_authorship(authorship: &DoctorForeignAuthorship) -> String {
    match authorship {
        DoctorForeignAuthorship::CurrentMachine { machine } => {
            format!(
                "authored by current machine {} (action required)",
                machine.name.as_str()
            )
        }
        DoctorForeignAuthorship::NonCurrentMachine { machine_id } => format!(
            "authored by non-current machine {}; inert, no repair action",
            machine_id.as_str()
        ),
        DoctorForeignAuthorship::Peer { peer_id } => format!(
            "authored by peer {}; inert, no repair action",
            peer_id.as_str()
        ),
        DoctorForeignAuthorship::ApiToken { token_id } => format!(
            "authored by API token {}; inert, no repair action",
            token_id.as_str()
        ),
        DoctorForeignAuthorship::Unparseable => {
            "authorship unparseable; inert, no repair action".to_owned()
        }
    }
}

fn render_unreachable(error: &OperatorRemoteError) -> String {
    format!(
        "cluster\tunreachable\n  {error}\n  check: `ployz network status`\n  if this operator peer was removed, re-join it with `ployz init root@<host>`\n"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::commands::{Command, MachineCommand, MachineUpgradeSelector};
    use crate::remote::ContextSelectionError;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const MACHINE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const MACHINE_C: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const MACHINE_D: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    const ROW_WINNER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB0";
    const ROW_LOSER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB1";
    const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB2";

    fn status_fixture() -> StatusDocument {
        serde_json::from_value(json!({
            "cluster": {
                "id": CLUSTER,
                "name": "acme-prod",
                "machine_count": 4
            },
            "answering_machine": {
                "state": "known",
                "id": MACHINE_A,
                "name": "core-1"
            },
            "sync": {
                "state": "syncing",
                "gaps": 2,
                "queue_size": 3,
                "p99_lag": 1.25
            },
            "barrier": "ready",
            "machines": [
                {
                    "id": MACHINE_A,
                    "name": "core-1",
                    "address": "fd00::1",
                    "handshake": { "state": "self_machine" }
                },
                {
                    "id": MACHINE_B,
                    "name": "edge-a",
                    "address": "fd00::2",
                    "handshake": {
                        "state": "no_testimony"
                    }
                },
                {
                    "id": MACHINE_C,
                    "name": "edge-b",
                    "address": "fd00::3",
                    "handshake": {
                        "state": "ago",
                        "seconds": 360,
                        "freshness": "stale"
                    }
                },
                {
                    "id": MACHINE_D,
                    "name": "edge-c",
                    "address": "fd00::4",
                    "handshake": { "state": "never" }
                }
            ],
            "hints": ["all_peer_handshakes_stale"]
        }))
        .expect("literal status DTO")
    }

    fn doctor_fixture() -> DoctorDocument {
        serde_json::from_value(json!({
            "skipped_roster_rows": [],
            "skipped_newer_versions": [
                {
                    "table": "services",
                    "key": ROW_LOSER,
                    "found": 2,
                    "supported": 1
                }
            ],
            "versions": {
                "newest": {
                    "version": "0.2.0",
                    "machines": [
                        { "id": MACHINE_A, "name": "core-1" }
                    ]
                },
                "behind": [
                    {
                        "machine": { "id": MACHINE_B, "name": "edge-a" },
                        "version": "0.1.0"
                    }
                ],
                "invalid": []
            },
            "foreign_clusters": [
                {
                    "cluster_id": "01ARZ3NDEKTSV4RRFFQ69G5FB3",
                    "rows": [
                        {
                            "table": "services",
                            "key": ROW_LOSER,
                            "authorship": {
                                "kind": "current_machine",
                                "machine": { "id": MACHINE_B, "name": "edge-a" }
                            }
                        },
                        {
                            "table": "tokens",
                            "key": ROW_WINNER,
                            "authorship": { "kind": "peer", "peer_id": PEER }
                        },
                        {
                            "table": "operations",
                            "key": "operation/orphan",
                            "authorship": {
                                "kind": "non_current_machine",
                                "machine_id": MACHINE_C
                            }
                        },
                        {
                            "table": "containers",
                            "key": "container/broken",
                            "authorship": { "kind": "unparseable" }
                        }
                    ]
                }
            ]
        }))
        .expect("literal doctor DTO")
    }

    #[test]
    fn status_renders_stable_summary_handshake_evidence_and_handoffs() {
        let output = render_status(&status_fixture());

        assert!(output.contains(&format!("cluster\tacme-prod\t{CLUSTER}\t4 machines")));
        assert!(output.contains("sync\tsyncing\tgaps 2, queue 3, lag p99 1.25\t"));
        assert!(!output.contains("lag p99 1.25s"));
        assert!(output.contains("(as seen from core-1)"));
        assert!(output.contains("barrier\tready"));
        assert!(output.contains("NAME\tHANDSHAKE\tADDR"));
        assert!(!output.contains("ROLE"));
        assert!(output.contains("core-1\tself\tfd00::1"));
        assert!(output.contains("edge-a\tno testimony\tfd00::2"));
        assert!(output.contains("edge-b\t360s ago ⚠\tfd00::3"));
        assert!(output.contains("edge-c\tnever\tfd00::4"));
        assert!(output.contains("`ployz network status`"));
        assert!(output.contains("`ployz doctor`"));

        let mut caught_up = status_fixture();
        caught_up.sync = StatusSync::CaughtUp { p99_lag: 0.48 };
        let caught_up = render_status(&caught_up);
        assert!(caught_up.contains("sync\tcaught up\tlag p99 0.48\t"));
        assert!(!caught_up.contains("lag p99 0.48s"));
    }

    #[test]
    fn handshake_rendering_preserves_exact_seconds_across_the_stale_boundary() {
        assert_eq!(
            render_handshake(&StatusHandshakeEvidence::Ago {
                seconds: 275,
                freshness: HandshakeFreshness::Healthy,
            }),
            "275s ago"
        );
        assert_eq!(
            render_handshake(&StatusHandshakeEvidence::Ago {
                seconds: 276,
                freshness: HandshakeFreshness::Stale,
            }),
            "276s ago ⚠"
        );
        assert_eq!(
            render_handshake(&StatusHandshakeEvidence::Ago {
                seconds: 86_401,
                freshness: HandshakeFreshness::Stale,
            }),
            "86401s ago ⚠"
        );
    }

    #[test]
    fn no_roster_names_the_only_join_door() {
        let document: StatusDocument = serde_json::from_value(json!({
            "cluster": null,
            "answering_machine": { "state": "unknown", "id": MACHINE_A },
            "sync": { "state": "no_lag_sample" },
            "barrier": "no_roster",
            "machines": [],
            "hints": []
        }))
        .expect("literal no-roster status DTO");

        let output = render_status(&document);
        assert!(output.contains("cluster\tunknown"));
        assert!(output.contains("sync\tno lag sample"));
        assert!(output.contains("barrier\tno roster"));
        assert!(output.contains("`ployz machine join <token>`"));
    }

    #[test]
    fn foreign_machine_repair_parses_an_option_looking_exact_name() {
        let mut document = doctor_fixture();
        for foreign in &mut document.foreign_clusters {
            for row in &mut foreign.rows {
                if let DoctorForeignAuthorship::CurrentMachine { machine } = &mut row.authorship {
                    machine.name = ployz_core::machine::MachineName::try_new("--help")
                        .expect("option-looking machine name");
                }
            }
        }

        let (output, has_findings) = render_doctor(&document);

        assert!(has_findings);
        assert!(output.contains("ployz machine rm -- '--help'"));
        assert!(matches!(
            crate::commands::parse_command(
                ["machine", "rm", "--", "--help"].into_iter().map(ToOwned::to_owned),
            ),
            Ok(Command::Machine(MachineCommand::Remove(command)))
                if command.machine.as_str() == "--help"
        ));
    }

    #[test]
    fn inert_foreign_cluster_rows_are_still_doctor_findings() {
        let mut document = doctor_fixture();
        document.skipped_newer_versions.clear();
        document.versions.behind.clear();
        document.versions.invalid.clear();
        for foreign in &mut document.foreign_clusters {
            foreign.rows.retain(|row| {
                !matches!(
                    &row.authorship,
                    DoctorForeignAuthorship::CurrentMachine { .. }
                )
            });
        }

        let (output, has_findings) = render_doctor(&document);

        assert!(has_findings);
        assert!(output.starts_with("doctor\tfindings\n"));
        assert!(!output.contains("doctor\tclean"));
        assert!(output.contains("inert, no repair action"));
    }

    #[test]
    fn noncanonical_roster_row_names_its_exact_expected_key() {
        let mut document = doctor_fixture();
        document.skipped_roster_rows = vec![ployz_core::DoctorSkippedRosterRow {
            table: DoctorRosterTable::Machines,
            key: "wrong-key".to_owned(),
            reason: DoctorRosterRowSkipReason::InvalidRowKey {
                expected: "edge-a".to_owned(),
            },
        }];

        let (output, has_findings) = render_doctor(&document);

        assert!(has_findings);
        assert!(output.contains(
            "skipped roster row: machine wrong-key (noncanonical row key; expected edge-a)"
        ));
    }

    #[test]
    fn skipped_newer_rows_with_a_lagging_machine_name_the_upgrade_handoff() {
        let document: DoctorDocument = serde_json::from_value(json!({
            "skipped_newer_versions": [
                {
                    "table": "services",
                    "key": ROW_LOSER,
                    "found": 2,
                    "supported": 1
                }
            ],
            "versions": {
                "newest": {
                    "version": "0.2.0",
                    "machines": [
                        { "id": MACHINE_A, "name": "core-1" }
                    ]
                },
                "behind": [
                    {
                        "machine": { "id": MACHINE_B, "name": "--all" },
                        "version": "0.1.0"
                    },
                    {
                        "machine": { "id": MACHINE_C, "name": "--help" },
                        "version": "0.1.0"
                    }
                ],
                "invalid": []
            },
            "foreign_clusters": []
        }))
        .expect("literal skipped-newer doctor DTO");

        let (output, has_findings) = render_doctor(&document);
        assert!(has_findings);
        assert!(output.contains(&format!(
            "newer row version: services {ROW_LOSER} has v=2 (this binary supports v=1)"
        )));
        assert!(output.contains("ployz machine upgrade -- '--all' '--help'"));

        let command = crate::commands::parse_command(
            ["machine", "upgrade", "--", "--all", "--help"]
                .into_iter()
                .map(ToOwned::to_owned),
        )
        .expect("option-looking machine names parse after the separator");
        let Command::Machine(MachineCommand::Upgrade(command)) = command else {
            panic!("doctor upgrade repair parses as machine upgrade");
        };
        let MachineUpgradeSelector::Names(names) = command.selector else {
            panic!("doctor upgrade repair selects exact machine names");
        };
        let [first, second] = names.as_slice() else {
            panic!("doctor upgrade repair keeps both exact machine names");
        };
        assert_eq!(first.as_str(), "--all");
        assert_eq!(second.as_str(), "--help");
    }

    #[test]
    fn clean_findings_and_unreachable_have_distinct_exit_outcomes() {
        let clean: DoctorDocument = serde_json::from_value(json!({
            "skipped_newer_versions": [],
            "versions": { "newest": null, "behind": [], "invalid": [] },
            "foreign_clusters": []
        }))
        .expect("literal clean doctor DTO");
        let (clean_output, clean_has_findings) = render_doctor(&clean);
        let clean = if clean_has_findings {
            DiagnosticsOutcome::findings(clean_output)
        } else {
            DiagnosticsOutcome::report(clean_output)
        };
        assert!(clean.is_success());
        assert!(!clean.is_unreachable());
        assert!(clean.output().contains("doctor\tclean"));

        let (findings_output, findings) = render_doctor(&doctor_fixture());
        let findings = if findings {
            DiagnosticsOutcome::findings(findings_output)
        } else {
            DiagnosticsOutcome::report(findings_output)
        };
        assert!(!findings.is_success());
        assert!(!findings.is_unreachable());

        let unreachable = DiagnosticsOutcome::unreachable(&OperatorRemoteError::Selection(
            ContextSelectionError::NotConfigured,
        ));
        assert!(!unreachable.is_success());
        assert!(unreachable.is_unreachable());
        assert!(unreachable.output().contains("cluster\tunreachable"));
        assert!(unreachable.output().contains("`ployz network status`"));
        assert!(unreachable.output().contains("re-join"));
    }
}
