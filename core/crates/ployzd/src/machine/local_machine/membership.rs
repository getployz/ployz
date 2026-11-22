//! Membership as this Machine sees it: the Machines it lists, with the membership and
//! round trips its admin socket and mesh report.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};

use ployz_core::{
    Machine, MachineId, MachineIdentity, MachineList, MachineObservation, RttObservation,
    RttStatistics, SelectedEndpoint, synthesize_membership,
};

use crate::corrosion::{AdminClient, IO_TIMEOUT, MembershipStates, ReplicatedStore};

use super::{Error, LocalMachine, published_network};

impl LocalMachine {
    /// Entry-local membership and Corrosion's RTT from the admin socket, with the mesh
    /// echo's RTT from `paths`.
    ///
    /// Returns `None` when the admin socket cannot be read so Watch can keep
    /// replicated rows.
    pub(crate) async fn runtime_watch_telemetry(
        &self,
        paths: &crate::network::PathBook,
    ) -> Option<RuntimeWatchTelemetry> {
        let Some(cluster) = &self.cluster else {
            return None;
        };
        let local = self.record();
        let (states, rtts) = read_admin(&cluster.admin).await?;
        Some(RuntimeWatchTelemetry {
            states,
            rtts,
            mesh_rtts: paths.rtts(),
            selected_endpoints: local.selected_endpoints.clone(),
        })
    }

    /// Membership Observation of Machines visible from this participating Machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotParticipating`] when this Machine is not
    /// participating, [`Error::ClusterStoreUnavailable`] when the Cluster
    /// store is missing, [`Error::ClusterNetworkUnpublished`] before the
    /// founder publishes the Cluster network, [`Error::ClusterStoreEmpty`] when
    /// this joined Machine's store never held another Machine, and [`Error::Cluster`] when
    /// replicated I/O fails.
    pub async fn list_machines(&self) -> Result<MachineList, Error> {
        let local = self.record();
        local.require_participating()?;
        let replicated = self.replicated()?;
        let network = published_network(&local, replicated).await?;
        let machines = replicated.machines().await?.observations;
        let states = self.membership_states().await?;
        let entry_id = local.id();
        // A listing never fails for the declaration: without it, readers infer the Doors.
        let doors = match replicated.cluster_doors().await {
            Ok(declared) => declared.followed(),
            Err(error) => {
                tracing::warn!(
                    error = %ployz_core::error_chain::inline(&error),
                    "could not read the Cluster's declared Doors; listing without them"
                );
                None
            }
        };
        Ok(MachineList {
            enrollment: Some(ployz_core::EnrollmentSnapshot {
                network,
                machines: machines.clone(),
                target_versions: replicated.version().await?,
                doors,
            }),
            machines: RuntimeWatchTelemetry {
                states,
                selected_endpoints: local.selected_endpoints.clone(),
                rtts: Vec::new(),
                mesh_rtts: BTreeMap::new(),
            }
            .overlay(machines, &entry_id),
        })
    }

    async fn membership_states(&self) -> Result<MembershipStates, Error> {
        Ok(match &self.cluster {
            Some(cluster) => cluster.admin.membership(IO_TIMEOUT).await?,
            None => MembershipStates::new(),
        })
    }
}

/// Entry-local admin membership, RTT samples, and selected endpoints.
///
/// Projected onto the current replicated Machine snapshot at assemble time.
/// Missing telemetry is not a delete of the replicated Machine.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RuntimeWatchTelemetry {
    pub states: MembershipStates,
    pub selected_endpoints: BTreeMap<MachineId, SelectedEndpoint>,
    /// Corrosion's RTT, used for a Machine with no mesh echo RTT.
    pub rtts: Vec<RttObservation>,
    /// The mesh echo's RTT per Machine, as ListMachines and Inspect report it (#3502).
    pub mesh_rtts: BTreeMap<MachineId, RttStatistics>,
}

impl RuntimeWatchTelemetry {
    /// Membership, selected endpoints, and RTT for the current replicated Machines.
    pub(crate) fn overlay(
        &self,
        machines: Vec<Machine>,
        entry_id: &MachineId,
    ) -> Vec<MachineObservation> {
        let rtt = rtts_by_machine(&machines, &self.rtts);
        let mut observations = synthesize_membership(machines, entry_id, &self.states);
        for observation in &mut observations {
            observation.selected_endpoint = self
                .selected_endpoints
                .get(&observation.machine.id)
                .copied();
            observation.rtt = self
                .mesh_rtts
                .get(&observation.machine.id)
                .or_else(|| rtt.get(&observation.machine.id))
                .cloned();
        }
        observations
    }
}

pub(super) async fn machine_rtts(
    admin: &AdminClient,
    replicated: &ReplicatedStore,
) -> Result<Vec<RttObservation>, crate::corrosion::Error> {
    let machines = replicated.machines().await?.observations;
    let identities = unique_identities(machines.into_iter().map(|machine| {
        (
            IpAddr::V6(machine.management_address().0),
            MachineIdentity {
                id: machine.id,
                name: machine.name,
            },
        )
    }));
    Ok(admin
        .member_rtts()
        .await?
        .into_iter()
        .map(|mut observation| {
            observation.machine = identities.get(&observation.address.ip()).cloned();
            observation
        })
        .collect())
}

/// Read membership and RTT from the local admin socket.
///
/// Both reads must succeed; otherwise Watch keeps replicated rows.
async fn read_admin(admin: &AdminClient) -> Option<(MembershipStates, Vec<RttObservation>)> {
    let Ok((states, rtts)) = tokio::try_join!(admin.membership(IO_TIMEOUT), admin.member_rtts())
    else {
        return None;
    };
    Some((states, rtts))
}

fn rtts_by_machine(
    machines: &[Machine],
    rtts: &[RttObservation],
) -> BTreeMap<MachineId, RttStatistics> {
    let identities = unique_identities(
        machines
            .iter()
            .map(|machine| (IpAddr::V6(machine.management_address().0), machine.id)),
    );
    rtts.iter()
        .filter_map(|observation| {
            identities
                .get(&observation.address.ip())
                .copied()
                .map(|id| (id, observation.statistics.clone()))
        })
        .collect()
}

/// Associate one value per management address; duplicate addresses are dropped.
fn unique_identities<T>(entries: impl IntoIterator<Item = (IpAddr, T)>) -> BTreeMap<IpAddr, T> {
    let mut identities = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for (address, identity) in entries {
        if identities.insert(address, identity).is_some() {
            ambiguous.insert(address);
        }
    }
    identities.retain(|address, _| !ambiguous.contains(address));
    identities
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::corrosion::AdminClient;
    use ployz_core::{
        AdvertisedEndpoint, CORROSION_GOSSIP_PORT, Machine, MachineId, MachineIdentity,
        MachineName, MachineRuntime, MembershipEvidence, MembershipObservation, RttObservation,
        RttStatistics, SelectedEndpoint, WireGuardPublicKey,
    };

    const ENTRY_ID: &str = "0123456789abcdef0123456789abcdef";
    const PEER_ID: &str = "fedcba9876543210fedcba9876543210";

    #[test]
    fn duplicate_management_addresses_have_no_identity_winner() {
        let duplicate = "192.0.2.1".parse().unwrap();
        let unique = "192.0.2.2".parse().unwrap();
        let identity = |seed: char| MachineIdentity {
            id: MachineId::parse(seed.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(seed.to_string()).unwrap(),
        };
        let identities = unique_identities([
            (duplicate, identity('1')),
            (duplicate, identity('2')),
            (unique, identity('3')),
        ]);

        assert!(!identities.contains_key(&duplicate));
        assert_eq!(identities.get(&unique), Some(&identity('3')));
    }

    #[test]
    fn overlay_marks_entry_up_and_maps_inspect_rtt() {
        let entry = machine("edge", ENTRY_ID, 1);
        let peer = machine("peer", PEER_ID, 2);
        let endpoint = SelectedEndpoint("203.0.113.10:51820".parse().unwrap());
        let rtt = RttStatistics {
            median_ns: 1_500_000,
            population_stddev_ns: 250_000,
        };
        let observations = RuntimeWatchTelemetry {
            states: BTreeMap::from([(peer.management_address(), MembershipEvidence::Suspect)]),
            selected_endpoints: BTreeMap::from([(entry.id, endpoint)]),
            rtts: vec![RttObservation {
                peer_id: "peer".into(),
                address: format!("[{}]:{CORROSION_GOSSIP_PORT}", peer.management_address().0)
                    .parse()
                    .unwrap(),
                machine: None,
                statistics: rtt.clone(),
                path: None,
            }],
            mesh_rtts: BTreeMap::new(),
        }
        .overlay(vec![entry.clone(), peer], &entry.id);

        let [entry_row, peer_row] = observations.as_slice() else {
            panic!("expected entry and peer observations");
        };
        assert_eq!(entry_row.membership, MembershipObservation::Up);
        assert_eq!(entry_row.selected_endpoint, Some(endpoint));
        assert_eq!(peer_row.membership, MembershipObservation::Suspect);
        assert_eq!(peer_row.rtt, Some(rtt));
    }

    #[test]
    fn overlay_prefers_mesh_echo_rtt_over_corrosion() {
        let entry = machine("edge", ENTRY_ID, 1);
        let echoed = machine("echoed", PEER_ID, 2);
        let silent = machine("silent", &"c".repeat(32), 3);
        let corrosion = |machine: &Machine, median_ns| RttObservation {
            peer_id: machine.name.to_string(),
            address: format!(
                "[{}]:{CORROSION_GOSSIP_PORT}",
                machine.management_address().0
            )
            .parse()
            .unwrap(),
            machine: None,
            statistics: RttStatistics {
                median_ns,
                population_stddev_ns: 0,
            },
            path: None,
        };
        let mesh = RttStatistics {
            median_ns: 1_200_000,
            population_stddev_ns: 100_000,
        };
        let observations = RuntimeWatchTelemetry {
            rtts: vec![corrosion(&echoed, 0), corrosion(&silent, 7_000_000)],
            mesh_rtts: BTreeMap::from([(echoed.id, mesh.clone())]),
            ..RuntimeWatchTelemetry::default()
        }
        .overlay(vec![entry.clone(), echoed, silent], &entry.id);

        let [_, echoed_row, silent_row] = observations.as_slice() else {
            panic!("expected entry and two peer observations");
        };
        assert_eq!(echoed_row.rtt, Some(mesh));
        assert_eq!(
            silent_row.rtt.as_ref().map(|rtt| rtt.median_ns),
            Some(7_000_000)
        );
    }

    #[test]
    fn overlay_uses_down_for_peers_missing_from_admin() {
        let entry = machine("edge", ENTRY_ID, 1);
        let peer = machine("peer", PEER_ID, 2);
        let observations =
            RuntimeWatchTelemetry::default().overlay(vec![entry.clone(), peer], &entry.id);

        let [entry_row, peer_row] = observations.as_slice() else {
            panic!("expected entry and peer observations");
        };
        assert_eq!(entry_row.membership, MembershipObservation::Up);
        assert_eq!(peer_row.membership, MembershipObservation::Down);
        assert_eq!(peer_row.rtt, None);
    }

    #[tokio::test]
    async fn missing_admin_socket_is_unavailable_telemetry() {
        let admin = AdminClient::new("/no/such/ployz-admin.sock");
        assert!(read_admin(&admin).await.is_none());
    }

    fn machine(name: &str, id: &str, seed: u8) -> Machine {
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::parse(id).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
            public_key: WireGuardPublicKey([seed; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint(
                format!("203.0.113.{seed}:51820").parse().unwrap(),
            )],
            runtime: MachineRuntime::default(),
            join_approval: None,
            build_concurrency: None,
        }
    }
}
