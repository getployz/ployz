//! The reserved Ingress doors as Docker shows them: the holder that owns the Ingress
//! Proxy's network namespace, the Caddy that joins it, and the Tunnel Origin with the
//! Tunnel Connector that joins it.

use ployz_core::labels;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use std::collections::HashMap;

use bollard::models::{
    ContainerInspectResponse, ContainerStateStatusEnum, NetworkCreateRequest, NetworkInspect,
};
use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation, MachineId,
    Namespace, PullPolicy, QualifiedService, ServiceName,
};

use super::{
    ContainerRuntime, Error, Status, TUNNEL_NETWORK_NAME, classify, docker_error,
    lifecycle::{failed_start, is_running},
};
use crate::docker_image::prepare_image;

/// Which reserved Ingress identity a Container has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Door {
    Holder,
    Proxy,
    TunnelOrigin,
    Tunnel,
}

impl Door {
    pub(crate) const ALL: [Self; 4] = [Self::Holder, Self::Proxy, Self::TunnelOrigin, Self::Tunnel];

    /// The door the Service `name` in `namespace` is, whatever its Containers' labels say.
    #[must_use]
    pub(crate) fn of_service(namespace: &Namespace, name: &ServiceName) -> Option<Self> {
        Self::of(&QualifiedService::new(namespace.clone(), name.clone()))
    }

    #[must_use]
    pub(crate) fn of(identity: &QualifiedService) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|door| door.identity() == *identity)
    }

    /// The door a Container's labels name, read as they are: a Container whose labels
    /// do not parse as a Service's still names its door.
    #[must_use]
    pub(crate) fn of_labels(labels: &std::collections::HashMap<String, String>) -> Option<Self> {
        Self::ALL.into_iter().find(|door| {
            let identity = door.identity();
            labels.get(labels::NAMESPACE) == Some(&identity.namespace.to_string())
                && labels.get(labels::SERVICE_NAME) == Some(&identity.name.to_string())
        })
    }

    #[must_use]
    pub(crate) fn identity(self) -> QualifiedService {
        match self {
            Self::Holder => QualifiedService::system_ingress_holder(),
            Self::Proxy => QualifiedService::system_ingress(),
            Self::TunnelOrigin => QualifiedService::system_tunnel_origin(),
            Self::Tunnel => QualifiedService::system_tunnel(),
        }
    }

    /// Whether other doors join this one's network namespace: the holder's Caddy, or
    /// the Tunnel Origin's connector.
    #[must_use]
    pub(crate) fn owns_netns(self) -> bool {
        matches!(self, Self::Holder | Self::TunnelOrigin)
    }
}

/// A network namespace as procfs identifies it. Two processes share one when their
/// `/proc/<pid>/ns/net` links name the same inode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NetnsId {
    dev: u64,
    ino: u64,
}

#[cfg(test)]
impl NetnsId {
    pub(crate) const fn new(ino: u64) -> Self {
        Self { dev: 0, ino }
    }
}

/// What the daemon knows of one reserved Ingress Container, from its observation and
/// its Docker inspect.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DoorFacts {
    pub(crate) id: ContainerId,
    pub(crate) door: Door,
    pub(crate) created_at: i64,
    pub(crate) runtime: ContainerRuntimeObservation,
    /// Docker left it at rest with a start error recorded.
    pub(crate) failed_start: bool,
    /// The Container whose network it was created in, for a Caddy in `container:` mode.
    pub(crate) joined_to: Option<ContainerId>,
    /// Its network namespace while it runs, when procfs could be read.
    pub(crate) netns: Option<NetnsId>,
}

impl DoorFacts {
    pub(crate) fn running(&self) -> bool {
        matches!(self.runtime, ContainerRuntimeObservation::Running { .. })
    }

    pub(crate) fn at_rest(&self) -> bool {
        self.runtime.at_rest()
    }
}

/// A reserved Ingress Container with its stored spec and its Docker facts.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DoorEntry {
    pub(crate) observation: ContainerObservation,
    pub(crate) facts: DoorFacts,
}

/// The Container a `container:` network mode names.
pub(crate) fn joined_to(inspected: &ContainerInspectResponse) -> Option<ContainerId> {
    inspected
        .host_config
        .as_ref()?
        .network_mode
        .as_deref()?
        .strip_prefix("container:")
        .and_then(|id| ContainerId::parse(id).ok())
}

/// The network namespace of a running Container's process.
pub(crate) fn netns_of(inspected: &ContainerInspectResponse) -> Option<NetnsId> {
    netns_at(Path::new("/proc"), inspected)
}

fn netns_at(proc_root: &Path, inspected: &ContainerInspectResponse) -> Option<NetnsId> {
    let state = inspected.state.as_ref()?;
    if state.status != Some(ContainerStateStatusEnum::RUNNING) {
        return None;
    }
    let pid = state.pid.filter(|pid| *pid > 0)?;
    let metadata = std::fs::metadata(proc_root.join(pid.to_string()).join("ns/net")).ok()?;
    Some(NetnsId {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

/// The Docker facts of a door Container, from the observation and typed inspect.
pub(crate) fn door_facts(
    observation: &ContainerObservation,
    door: Door,
    inspected: &ContainerInspectResponse,
) -> DoorFacts {
    DoorFacts {
        id: observation.container_id,
        door,
        created_at: observation.created_at_unix_nanos,
        runtime: observation.runtime.clone(),
        failed_start: failed_start(inspected),
        joined_to: joined_to(inspected),
        netns: netns_of(inspected),
    }
}

impl ContainerRuntime {
    /// Every reserved Ingress Container here, with its stored spec and Docker facts. One
    /// whose spec is missing is skipped with a log, so a half-made one blocks nothing.
    pub(crate) async fn door_inventory(
        &self,
        machine_id: &MachineId,
    ) -> Result<Vec<DoorEntry>, Error> {
        let mut entries = Vec::new();
        for door in Door::ALL {
            for (id, _) in self.docker.door_containers(door).await? {
                let observation = match self.inspect_managed(&id, machine_id).await {
                    Ok(observation) => observation,
                    Err(Error::ContainerNotFound(_)) => continue,
                    Err(Error::SpecNotFound(id)) => {
                        eprintln!("Ingress door {id} has no stored spec, so the route ignores it");
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if observation.kind != ContainerKind::ServiceContainer {
                    continue;
                }
                let Some(inspected) = self.inspect_present(&id).await? else {
                    continue;
                };
                entries.push(DoorEntry {
                    facts: door_facts(&observation, door, &inspected),
                    observation,
                });
            }
        }
        Ok(entries)
    }

    /// Pulls one of the daemon's pinned images when it is missing.
    pub(crate) async fn prepare_pinned_image(&self, image: &str) -> Result<(), Error> {
        prepare_image(&self.docker.client, image, PullPolicy::Missing, None)
            .await
            .map_err(Error::ImagePull)
    }

    /// The newest running Container of `door`, which a joined door is created in.
    pub(super) async fn running_door(&self, door: Door) -> Result<Option<ContainerId>, Error> {
        let mut running = Vec::new();
        for (id, _) in self.docker.door_containers(door).await? {
            let Some(inspected) = self.inspect_present(&id).await? else {
                continue;
            };
            if is_running(&inspected) {
                running.push((inspected.created.unwrap_or_default(), id));
            }
        }
        Ok(running.into_iter().max().map(|(_, id)| id))
    }

    /// Docker's inspect of `id`, or `None` once Docker no longer has it.
    async fn inspect_present(
        &self,
        id: &ContainerId,
    ) -> Result<Option<ContainerInspectResponse>, Error> {
        match self
            .docker
            .client
            .inspect_container(id.as_str(), None)
            .await
        {
            Ok(inspected) => Ok(Some(inspected)),
            Err(error) => {
                let error = docker_error(id, error);
                if matches!(error, Error::ContainerNotFound(_)) {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
        }
    }
}

/// Bollard's option that names a bridge network's interface, which the Machine's
/// firewall matches the Tunnel Door's traffic by.
const BRIDGE_NAME_OPTION: &str = "com.docker.network.bridge.name";

fn tunnel_network_request() -> NetworkCreateRequest {
    NetworkCreateRequest {
        name: TUNNEL_NETWORK_NAME.into(),
        driver: Some("bridge".into()),
        scope: Some("local".into()),
        enable_ipv6: Some(false),
        options: Some(HashMap::from([
            (BRIDGE_NAME_OPTION.into(), TUNNEL_NETWORK_NAME.into()),
            (
                "com.docker.network.bridge.enable_icc".into(),
                "false".into(),
            ),
        ])),
        labels: Some(labels::system_labels()),
        ..Default::default()
    }
}

/// Whether `network` is a bridge the firewall fences: one backed by the interface the
/// fence names, with no IPv6.
fn tunnel_network_fenced(network: &NetworkInspect) -> Result<(), Error> {
    let bridge = network.driver.as_deref() == Some("bridge")
        && network
            .options
            .as_ref()
            .and_then(|options| options.get(BRIDGE_NAME_OPTION))
            .map(String::as_str)
            == Some(TUNNEL_NETWORK_NAME);
    if bridge && network.enable_ipv6 != Some(true) {
        Ok(())
    } else {
        Err(Error::ForeignTunnelNetwork)
    }
}

impl super::LocalDocker {
    /// Make the Tunnel Origin's network when it is missing. A network of that name
    /// that another interface backs, or that takes IPv6, is refused rather than joined,
    /// since the firewall would not fence it.
    pub(super) async fn ensure_tunnel_network(&self) -> Result<(), Error> {
        match self.client.inspect_network(TUNNEL_NETWORK_NAME, None).await {
            Ok(network) => return tunnel_network_fenced(&network),
            Err(error) if classify(&error) == Status::Absent => {}
            Err(error) => return Err(Error::Docker(error)),
        }
        match self.client.create_network(tunnel_network_request()).await {
            Ok(_) => Ok(()),
            Err(error) if classify(&error) == Status::Conflict => tunnel_network_fenced(
                &self
                    .client
                    .inspect_network(TUNNEL_NETWORK_NAME, None)
                    .await?,
            ),
            Err(error) => Err(Error::Docker(error)),
        }
    }

    pub(super) async fn door_containers(
        &self,
        door: Door,
    ) -> Result<Vec<(ContainerId, std::collections::HashMap<String, String>)>, Error> {
        let identity = door.identity();
        self.labelled(vec![
            labels::MANAGED.to_owned(),
            format!("{}={}", labels::NAMESPACE, identity.namespace),
            format!("{}={}", labels::SERVICE_NAME, identity.name),
        ])
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerState, HostConfig};

    fn network(driver: &str, bridge: &str, ipv6: bool) -> NetworkInspect {
        NetworkInspect {
            driver: Some(driver.into()),
            enable_ipv6: Some(ipv6),
            options: Some(HashMap::from([(BRIDGE_NAME_OPTION.into(), bridge.into())])),
            ..Default::default()
        }
    }

    #[test]
    fn the_tunnel_origin_joins_only_a_network_the_firewall_fences() {
        let made = tunnel_network_request();
        assert_eq!(made.name, TUNNEL_NETWORK_NAME);
        assert!(
            tunnel_network_fenced(&NetworkInspect {
                driver: made.driver,
                enable_ipv6: made.enable_ipv6,
                options: made.options,
                ..Default::default()
            })
            .is_ok()
        );
        for foreign in [
            network("bridge", "br-0123456789ab", false),
            network("bridge", TUNNEL_NETWORK_NAME, true),
            network("macvlan", TUNNEL_NETWORK_NAME, false),
        ] {
            assert!(
                matches!(
                    tunnel_network_fenced(&foreign),
                    Err(Error::ForeignTunnelNetwork)
                ),
                "{foreign:?}"
            );
        }
    }

    fn inspected(
        status: ContainerStateStatusEnum,
        pid: i64,
        mode: &str,
    ) -> ContainerInspectResponse {
        ContainerInspectResponse {
            state: Some(ContainerState {
                status: Some(status),
                pid: Some(pid),
                ..Default::default()
            }),
            host_config: Some(HostConfig {
                network_mode: Some(mode.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_caddy_names_the_holder_it_joined() {
        let holder = ContainerId::parse("a".repeat(64)).unwrap();
        assert_eq!(
            joined_to(&inspected(
                ContainerStateStatusEnum::RUNNING,
                1,
                &format!("container:{holder}")
            )),
            Some(holder)
        );
        assert_eq!(
            joined_to(&inspected(ContainerStateStatusEnum::RUNNING, 1, "ployz")),
            None
        );
        assert_eq!(
            joined_to(&inspected(
                ContainerStateStatusEnum::RUNNING,
                1,
                "container:short"
            )),
            None,
            "a name is not an id, and the daemon only ever creates by id"
        );
    }

    #[test]
    fn the_netns_is_read_from_procfs_only_for_a_running_pid() {
        let root = std::env::temp_dir().join(format!("ployzd-netns-{}", MachineId::random()));
        std::fs::create_dir_all(root.join("7/ns")).unwrap();
        std::fs::write(root.join("7/ns/net"), b"").unwrap();
        let running = netns_at(
            &root,
            &inspected(ContainerStateStatusEnum::RUNNING, 7, "ployz"),
        );
        assert!(running.is_some());
        assert_eq!(
            running,
            netns_at(
                &root,
                &inspected(ContainerStateStatusEnum::RUNNING, 7, "ployz")
            ),
            "the same inode reads the same"
        );
        assert_eq!(
            netns_at(
                &root,
                &inspected(ContainerStateStatusEnum::EXITED, 7, "ployz")
            ),
            None
        );
        assert_eq!(
            netns_at(
                &root,
                &inspected(ContainerStateStatusEnum::RUNNING, 8, "ployz")
            ),
            None,
            "an unreadable namespace is unknown, not equal"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn labels_name_their_door_by_namespace_and_service_alone() {
        let labelled = |identity: QualifiedService| {
            std::collections::HashMap::from([
                (labels::NAMESPACE.to_owned(), identity.namespace.to_string()),
                (labels::SERVICE_NAME.to_owned(), identity.name.to_string()),
            ])
        };
        for door in Door::ALL {
            assert_eq!(Door::of_labels(&labelled(door.identity())), Some(door));
        }
        let app = QualifiedService::new(
            ployz_core::Namespace::parse("app").unwrap(),
            QualifiedService::system_ingress().name,
        );
        assert_eq!(Door::of_labels(&labelled(app)), None);
        assert_eq!(Door::of_labels(&std::collections::HashMap::new()), None);
    }
}
