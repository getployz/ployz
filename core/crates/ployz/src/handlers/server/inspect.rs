use std::net::Ipv4Addr;

use clap::ArgMatches;
use ployz_core::{
    InspectMachineUpgradeRequest, InspectRequest, MachineObservation, MachineStorageObservation,
    MachineTarget, RpcErrorCode, op,
};
use serde::Serialize;
use serde_json::json;

use super::with_client;
use crate::{
    connect::{ConnectError, TARGET_RPC_TIMEOUT},
    handlers::{Error, leaf_matches},
    output::{self, Gaps, say},
};

pub(in crate::handlers) fn list(root: &ArgMatches) -> Result<(), Error> {
    with_client(root, |client| {
        Box::pin(async move {
            let mut machines = client.machines().await?;
            let storage = client.observe_machine_storage(&mut machines).await;
            let warning = daemon_skew_warning(&machines, env!("CARGO_PKG_VERSION"));
            let listed = machines
                .iter()
                .map(|observation| MachineObservationOutput {
                    gateway: observation.machine.subnet.gateway().0,
                    public_key: observation.machine.public_key.to_string(),
                    observation,
                })
                .collect::<Vec<_>>();
            let mut gaps = Gaps::default();
            gaps.extend(&storage.failures, &storage.omissions);
            let finished = output::finish_fanout("servers", &listed, &gaps, || {
                say!(
                    "ID\tNAME\tMEMBERSHIP\tSTORAGE\tSUBNET\tGATEWAY\tPUBLIC IP\tENDPOINTS\tHOSTNAME\tDAEMON\tDOCKER\tOS\tKERNEL\tARCH"
                );
                for observed in &machines {
                    let machine = &observed.machine;
                    say!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        machine.id,
                        machine.name,
                        observed.membership.as_str(),
                        format_storage(observed.storage),
                        machine.subnet,
                        machine.subnet.gateway().0,
                        machine
                            .public_ip
                            .map_or_else(|| "-".into(), |ip| ip.to_string()),
                        machine
                            .advertised_endpoints
                            .iter()
                            .map(|endpoint| endpoint.0.to_string())
                            .collect::<Vec<_>>()
                            .join(","),
                        machine.runtime.hostname,
                        machine.runtime.daemon_version,
                        machine.runtime.docker_version,
                        machine.runtime.os_pretty_name,
                        machine.runtime.kernel_version,
                        machine.runtime.architecture,
                    );
                }
            });
            if let Some(warning) = warning {
                eprintln!("{warning}");
            }
            finished
        })
    })
}

#[must_use]
fn daemon_skew_warning(machines: &[MachineObservation], cli_version: &str) -> Option<String> {
    let count = machines
        .iter()
        .filter(|observed| observed.machine.runtime.daemon_version != cli_version)
        .count();
    match count {
        0 => None,
        1 => Some(format!(
            "WARNING: 1 Server runs a daemon version different from CLI {cli_version}."
        )),
        count => Some(format!(
            "WARNING: {count} Servers run daemon versions different from CLI {cli_version}."
        )),
    }
}

#[must_use]
fn format_storage(storage: Option<MachineStorageObservation>) -> String {
    match storage {
        None => "Volume support unknown".into(),
        Some(MachineStorageObservation::Stateless) => "Docker volumes only".into(),
        Some(MachineStorageObservation::Ready) => "Managed volumes available".into(),
        Some(MachineStorageObservation::Pool {
            size_bytes,
            used_bytes,
            free_bytes,
        }) => format!(
            "Managed volumes available ({size_bytes} bytes, {used_bytes} used, {free_bytes} free)"
        ),
    }
}

/// Print fresh targeted Machine telemetry, its round-trip times to peers, and its
/// latest upgrade attempt; list/watch request only storage evidence.
pub(in crate::handlers) fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let selector = MachineTarget::parse(
        leaf_matches(root)
            .get_one::<String>("server")
            .ok_or_else(|| Error::usage("server is required"))?,
    )?;
    with_client(root, |client| {
        Box::pin(async move {
            let details = client
                .invoke::<op::Inspect>(
                    InspectRequest {
                        telemetry: ployz_core::InspectTelemetry::Full,
                        include_rtts: true,
                        ..Default::default()
                    },
                    &selector,
                    Some(TARGET_RPC_TIMEOUT),
                )
                .await?;
            let upgrade = match client
                .call_repeatable::<op::InspectMachineUpgrade>(
                    InspectMachineUpgradeRequest { attempt_id: None },
                    Some(&selector),
                )
                .await
            {
                Ok(attempt) => Some(attempt),
                Err(ConnectError::Remote(error)) if error.code == RpcErrorCode::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            output::show(&json!({ "server": details, "upgrade": upgrade }))
        })
    })
}

#[derive(Serialize)]
struct MachineObservationOutput<'a> {
    #[serde(flatten)]
    observation: &'a MachineObservation,
    gateway: Ipv4Addr,
    /// The WireGuard public key in base64, as `wg` prints it.
    public_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::MachineStorageObservation;

    #[test]
    fn storage_column_distinguishes_ready_without_a_pool() {
        assert_eq!(format_storage(None), "Volume support unknown");
        assert_eq!(
            format_storage(Some(MachineStorageObservation::Stateless)),
            "Docker volumes only"
        );
        assert_eq!(
            format_storage(Some(MachineStorageObservation::Ready)),
            "Managed volumes available"
        );
        assert_eq!(
            format_storage(Some(MachineStorageObservation::Pool {
                size_bytes: std::num::NonZeroU64::new(4_294_967_296).unwrap(),
                used_bytes: 3_865_470_566,
                free_bytes: 429_496_730,
            })),
            "Managed volumes available (4294967296 bytes, 3865470566 used, 429496730 free)"
        );
    }

    #[test]
    fn machine_json_projection_includes_the_derived_gateway() {
        let observation = MachineObservation::new(
            ployz_core::Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                id: "0".repeat(32).parse().unwrap(),
                name: "node-a".parse().unwrap(),
                subnet: "10.210.7.0/24".parse().unwrap(),
                public_key: ployz_core::WireGuardPublicKey([7; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                build_concurrency: None,
            },
            ployz_core::MembershipObservation::Up,
        );
        let output = serde_json::to_value(MachineObservationOutput {
            gateway: observation.machine.subnet.gateway().0,
            public_key: observation.machine.public_key.to_string(),
            observation: &observation,
        })
        .unwrap();
        assert_eq!(output.get("gateway").unwrap(), "10.210.7.1");
        assert_eq!(
            output.get("public_key").unwrap(),
            "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc="
        );
    }
}
