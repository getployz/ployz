use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};

use ployz_types::model::{
    DrainState, InstancePhase, MachineId, MachineMembership, MachineTopology, RoutingState,
};
use ployz_types::spec::Namespace;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsInstanceDiagnostic {
    pub service: String,
    pub instance_id: String,
    pub machine_id: String,
    pub topology: MachineTopology,
    pub slot_id: String,
    pub overlay_ip: Ipv4Addr,
}

impl DnsInstanceDiagnostic {
    #[must_use]
    pub fn txt_record(&self) -> String {
        let availability_zone = self
            .topology
            .availability_zone
            .as_ref()
            .map(|zone| zone.as_str())
            .unwrap_or("none");
        format!(
            "service={},instance={},machine={},region={},az={},slot={},ip={}",
            self.service,
            self.instance_id,
            self.machine_id,
            self.topology.region.as_str(),
            availability_zone,
            self.slot_id,
            self.overlay_ip
        )
    }
}

// ---------------------------------------------------------------------------
// DnsSnapshot
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct DnsSnapshot {
    /// namespace -> service -> sorted Vec of overlay IPs for ready instances.
    /// Two-level map avoids cloning `Namespace` + allocating a `String` on
    /// every lookup in the hot path.
    pub services: HashMap<Namespace, HashMap<String, Vec<Ipv4Addr>>>,
    /// overlay_ip -> namespace (reverse lookup for caller namespace detection).
    /// Intentionally IPv4-only: the mesh issues only IPv4 overlay addresses,
    /// so an IPv6 caller has no namespace and bare-name lookups will NXDOMAIN.
    pub ip_to_namespace: HashMap<Ipv4Addr, Namespace>,
    /// namespace -> sorted list of service names (for TXT _services queries)
    pub service_names: HashMap<Namespace, Vec<String>>,
    /// namespace -> sorted instance diagnostics for routable instances.
    pub instances: HashMap<Namespace, Vec<DnsInstanceDiagnostic>>,
}

impl DnsSnapshot {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            services: HashMap::new(),
            ip_to_namespace: HashMap::new(),
            service_names: HashMap::new(),
            instances: HashMap::new(),
        }
    }

    /// Look up overlay IPs for a service in a namespace without allocating.
    #[must_use]
    pub fn lookup_service(&self, namespace: &Namespace, service: &str) -> Option<&[Ipv4Addr]> {
        self.services
            .get(namespace)
            .and_then(|by_service| by_service.get(service))
            .map(Vec::as_slice)
    }

    #[must_use]
    pub fn lookup_instance(
        &self,
        namespace: &Namespace,
        service: &str,
        instance_id: &str,
    ) -> Option<Ipv4Addr> {
        self.instances.get(namespace).and_then(|instances| {
            instances
                .iter()
                .find(|instance| instance.service == service && instance.instance_id == instance_id)
                .map(|instance| instance.overlay_ip)
        })
    }

    #[must_use]
    pub fn has_service_instances(&self, namespace: &Namespace, service: &str) -> bool {
        self.lookup_service(namespace, service)
            .is_some_and(|ips| !ips.is_empty())
    }

    #[must_use]
    pub fn instance_txt_records(
        &self,
        namespace: &Namespace,
        service: Option<&str>,
    ) -> Vec<String> {
        let Some(instances) = self.instances.get(namespace) else {
            return Vec::new();
        };
        instances
            .iter()
            .filter(|instance| {
                service
                    .map(|service| instance.service == service)
                    .unwrap_or(true)
            })
            .map(DnsInstanceDiagnostic::txt_record)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// SharedDnsSnapshot — same Arc<RwLock<Arc<_>>> pattern as gateway
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SharedDnsSnapshot {
    inner: Arc<RwLock<Arc<DnsSnapshot>>>,
}

impl SharedDnsSnapshot {
    #[must_use]
    pub fn new(snapshot: DnsSnapshot) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(snapshot))),
        }
    }

    #[must_use]
    pub fn load(&self) -> Arc<DnsSnapshot> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn replace(&self, snapshot: DnsSnapshot) {
        *self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(snapshot);
    }
}

// ---------------------------------------------------------------------------
// Projection: RoutingState -> DnsSnapshot
// ---------------------------------------------------------------------------

pub fn project_dns(state: &RoutingState) -> DnsSnapshot {
    let mut services: HashMap<Namespace, HashMap<String, Vec<Ipv4Addr>>> = HashMap::new();
    let mut ip_to_namespace: HashMap<Ipv4Addr, Namespace> = HashMap::new();
    let mut service_names: HashMap<Namespace, Vec<String>> = HashMap::new();
    let mut instances: HashMap<Namespace, Vec<DnsInstanceDiagnostic>> = HashMap::new();
    let machines = state
        .machines
        .iter()
        .map(|machine| (machine.id.clone(), machine))
        .collect::<HashMap<MachineId, &MachineMembership>>();

    for instance in &state.instances {
        if instance.phase != InstancePhase::Ready || !instance.ready {
            continue;
        }
        if instance.drain_state != DrainState::None {
            continue;
        }
        let Some(overlay_ip) = instance.overlay_ip else {
            continue;
        };
        let Some(machine) = machines.get(&instance.machine_id) else {
            warn!(
                namespace = %instance.namespace,
                service = %instance.service,
                instance_id = %instance.instance_id,
                machine_id = %instance.machine_id,
                "skipping orphaned instance with missing machine record",
            );
            continue;
        };

        services
            .entry(instance.namespace.clone())
            .or_default()
            .entry(instance.service.clone())
            .or_default()
            .push(overlay_ip);
        ip_to_namespace.insert(overlay_ip, instance.namespace.clone());
        instances
            .entry(instance.namespace.clone())
            .or_default()
            .push(DnsInstanceDiagnostic {
                service: instance.service.clone(),
                instance_id: instance.instance_id.as_str().to_string(),
                machine_id: instance.machine_id.as_str().to_string(),
                topology: machine.topology.clone(),
                slot_id: instance.slot_id.as_str().to_string(),
                overlay_ip,
            });
    }

    // Sort IPs for deterministic ordering
    for by_service in services.values_mut() {
        for ips in by_service.values_mut() {
            ips.sort();
        }
    }

    // Build service_names from services keys
    for (namespace, by_service) in &services {
        let mut names: Vec<String> = by_service.keys().cloned().collect();
        names.sort();
        service_names.insert(namespace.clone(), names);
    }
    for instances in instances.values_mut() {
        instances.sort_by(|left, right| {
            left.service
                .cmp(&right.service)
                .then_with(|| left.instance_id.cmp(&right.instance_id))
        });
    }

    DnsSnapshot {
        services,
        ip_to_namespace,
        service_names,
        instances,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{
        DeployId, InstanceId, InstanceStatusRecord, MachineId, MachineLifecycle, MachineMembership,
        OverlayIp, PublicKey, RoutingState, SlotId, StorageParticipation,
    };
    use std::collections::BTreeMap;

    fn ready_instance(
        namespace: &str,
        service: &str,
        overlay_ip: Option<Ipv4Addr>,
    ) -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId::new("inst-1"),
            namespace: Namespace::new(namespace),
            service: service.into(),
            slot_id: SlotId::new("slot-1"),
            machine_id: MachineId::new("machine-1"),
            revision_hash: "abc".into(),
            deploy_id: DeployId::new("deploy-1"),
            docker_container_id: "container-1".into(),
            overlay_ip,
            backend_ports: BTreeMap::new(),
            phase: InstancePhase::Ready,
            ready: true,
            drain_state: DrainState::None,
            error: None,
            started_at: 0,
            updated_at: 0,
        }
    }

    fn empty_routing_state() -> RoutingState {
        RoutingState {
            machines: vec![machine_record("machine-1")],
            revisions: vec![],
            releases: vec![],
            instances: vec![],
        }
    }

    fn machine_record(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId::new(id),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp("fd00::1".parse().expect("valid overlay ip")),
            topology: MachineTopology::local(),
            region_role: ployz_types::model::RegionRole::HomeData,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
            storage_role: StorageParticipation::default_authority().into(),
            created_at: 1,
            updated_at: 1,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn project_empty_state() {
        let snapshot = project_dns(&empty_routing_state());
        assert!(snapshot.services.is_empty());
        assert!(snapshot.ip_to_namespace.is_empty());
        assert!(snapshot.service_names.is_empty());
        assert!(snapshot.instances.is_empty());
    }

    #[test]
    fn project_ready_instance_with_ip() {
        let ip = Ipv4Addr::new(10, 42, 1, 10);
        let mut state = empty_routing_state();
        state
            .instances
            .push(ready_instance("prod", "web", Some(ip)));

        let snapshot = project_dns(&state);
        let ns = Namespace::new("prod");
        assert_eq!(snapshot.lookup_service(&ns, "web"), Some([ip].as_slice()));
        assert_eq!(
            snapshot.ip_to_namespace.get(&ip),
            Some(&Namespace::new("prod"))
        );
        assert_eq!(
            snapshot
                .service_names
                .get(&Namespace::new("prod"))
                .map(Vec::as_slice),
            Some(vec!["web".to_string()].as_slice())
        );
        assert_eq!(
            snapshot.instance_txt_records(&ns, Some("web")),
            vec![
                "service=web,instance=inst-1,machine=machine-1,region=local,az=none,slot=slot-1,ip=10.42.1.10"
            ]
        );
        assert_eq!(snapshot.lookup_instance(&ns, "web", "inst-1"), Some(ip));
    }

    #[test]
    fn instance_diagnostics_include_region_and_availability_zone() {
        let ip = Ipv4Addr::new(10, 42, 1, 10);
        let mut state = empty_routing_state();
        state.machines = vec![MachineMembership {
            topology: MachineTopology::new("us-west", Some("usw1-a"))
                .expect("topology should parse"),
            ..machine_record("machine-1")
        }];
        state
            .instances
            .push(ready_instance("prod", "web", Some(ip)));

        let snapshot = project_dns(&state);
        let ns = Namespace::new("prod");

        assert_eq!(
            snapshot.instance_txt_records(&ns, Some("web")),
            vec![
                "service=web,instance=inst-1,machine=machine-1,region=us-west,az=usw1-a,slot=slot-1,ip=10.42.1.10"
            ]
        );
    }

    #[test]
    fn project_skips_orphan_instance_and_keeps_healthy_services() {
        let orphan_ip = Ipv4Addr::new(10, 42, 1, 10);
        let healthy_ip = Ipv4Addr::new(10, 42, 1, 11);
        let mut state = empty_routing_state();
        state.machines = vec![machine_record("machine-1")];
        let mut orphan = ready_instance("prod", "web", Some(orphan_ip));
        orphan.machine_id = MachineId::new("missing-machine");
        state.instances.push(orphan);
        let mut healthy = ready_instance("prod", "api", Some(healthy_ip));
        healthy.instance_id = InstanceId::new("inst-2");
        state.instances.push(healthy);

        let snapshot = project_dns(&state);
        let ns = Namespace::new("prod");
        assert_eq!(snapshot.lookup_service(&ns, "web"), None);
        assert_eq!(
            snapshot.lookup_service(&ns, "api"),
            Some([healthy_ip].as_slice())
        );
    }

    #[test]
    fn project_skips_not_ready() {
        let ip = Ipv4Addr::new(10, 42, 1, 10);
        let mut instance = ready_instance("prod", "web", Some(ip));
        instance.phase = InstancePhase::Starting;

        let mut state = empty_routing_state();
        state.instances.push(instance);

        let snapshot = project_dns(&state);
        assert!(snapshot.services.is_empty());
        assert!(snapshot.instances.is_empty());
    }

    #[test]
    fn project_skips_draining() {
        let ip = Ipv4Addr::new(10, 42, 1, 10);
        let mut instance = ready_instance("prod", "web", Some(ip));
        instance.drain_state = DrainState::Requested;

        let mut state = empty_routing_state();
        state.instances.push(instance);

        let snapshot = project_dns(&state);
        assert!(snapshot.services.is_empty());
        assert!(snapshot.instances.is_empty());
    }

    #[test]
    fn project_skips_no_overlay_ip() {
        let mut state = empty_routing_state();
        state.instances.push(ready_instance("prod", "web", None));

        let snapshot = project_dns(&state);
        assert!(snapshot.services.is_empty());
        assert!(snapshot.instances.is_empty());
    }
}
