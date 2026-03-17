use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};

use ployz_types::model::{DrainState, InstancePhase, RoutingState};
use ployz_types::spec::Namespace;

// ---------------------------------------------------------------------------
// DnsSnapshot
// ---------------------------------------------------------------------------

pub struct DnsSnapshot {
    /// namespace -> service -> sorted Vec of overlay IPs for ready instances.
    /// Two-level map avoids cloning `Namespace` + allocating a `String` on
    /// every lookup in the hot path.
    pub services: HashMap<Namespace, HashMap<String, Vec<Ipv4Addr>>>,
    /// overlay_ip -> namespace (reverse lookup for caller namespace detection)
    pub ip_to_namespace: HashMap<Ipv4Addr, Namespace>,
    /// namespace -> sorted list of service names (for TXT _services queries)
    pub service_names: HashMap<Namespace, Vec<String>>,
}

impl DnsSnapshot {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            services: HashMap::new(),
            ip_to_namespace: HashMap::new(),
            service_names: HashMap::new(),
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

#[must_use]
pub fn project_dns(state: &RoutingState) -> DnsSnapshot {
    let mut services: HashMap<Namespace, HashMap<String, Vec<Ipv4Addr>>> = HashMap::new();
    let mut ip_to_namespace: HashMap<Ipv4Addr, Namespace> = HashMap::new();
    let mut service_names: HashMap<Namespace, Vec<String>> = HashMap::new();

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

        services
            .entry(instance.namespace.clone())
            .or_default()
            .entry(instance.service.clone())
            .or_default()
            .push(overlay_ip);
        ip_to_namespace.insert(overlay_ip, instance.namespace.clone());
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

    DnsSnapshot {
        services,
        ip_to_namespace,
        service_names,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{
        DeployId, InstanceId, InstanceStatusRecord, MachineId, RoutingState, SlotId,
    };
    use std::collections::BTreeMap;

    fn ready_instance(
        namespace: &str,
        service: &str,
        overlay_ip: Option<Ipv4Addr>,
    ) -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId("inst-1".into()),
            namespace: Namespace(namespace.into()),
            service: service.into(),
            slot_id: SlotId("slot-1".into()),
            machine_id: MachineId("machine-1".into()),
            revision_hash: "abc".into(),
            deploy_id: DeployId("deploy-1".into()),
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
            revisions: vec![],
            releases: vec![],
            instances: vec![],
        }
    }

    #[test]
    fn project_empty_state() {
        let snapshot = project_dns(&empty_routing_state());
        assert!(snapshot.services.is_empty());
        assert!(snapshot.ip_to_namespace.is_empty());
        assert!(snapshot.service_names.is_empty());
    }

    #[test]
    fn project_ready_instance_with_ip() {
        let ip = Ipv4Addr::new(10, 42, 1, 10);
        let mut state = empty_routing_state();
        state
            .instances
            .push(ready_instance("prod", "web", Some(ip)));

        let snapshot = project_dns(&state);
        let ns = Namespace("prod".into());
        assert_eq!(snapshot.lookup_service(&ns, "web"), Some([ip].as_slice()));
        assert_eq!(
            snapshot.ip_to_namespace.get(&ip),
            Some(&Namespace("prod".into()))
        );
        assert_eq!(
            snapshot
                .service_names
                .get(&Namespace("prod".into()))
                .map(Vec::as_slice),
            Some(vec!["web".to_string()].as_slice())
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
    }

    #[test]
    fn project_skips_no_overlay_ip() {
        let mut state = empty_routing_state();
        state.instances.push(ready_instance("prod", "web", None));

        let snapshot = project_dns(&state);
        assert!(snapshot.services.is_empty());
    }
}
