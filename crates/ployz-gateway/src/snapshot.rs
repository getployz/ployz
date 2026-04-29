use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use crate::routes::{
    AcmeChallengeView, BackendView, CertificateView, GatewaySnapshot, HttpRouteView,
    ProjectionDelta, TcpRouteView, normalize_path_prefix, normalize_request_host,
};
use pingora::lb::prelude::LoadBalancer;
use pingora::lb::selection::RoundRobin;

pub struct SnapshotState {
    pub snapshot: Arc<GatewaySnapshot>,
    pub load_balancers: HashMap<String, Arc<LoadBalancer<RoundRobin>>>,
    pub backend_lookup: HashMap<SocketAddr, BackendView>,
    backend_ref_counts: HashMap<SocketAddr, usize>,
    http_routes: HashMap<String, Arc<HttpRouteView>>,
    tcp_routes: HashMap<String, Arc<TcpRouteView>>,
    http_by_host: HashMap<String, Vec<Arc<HttpRouteView>>>,
    http_wildcard: Vec<Arc<HttpRouteView>>,
    acme_challenges: HashMap<(String, String), AcmeChallengeView>,
    certificates: HashMap<String, CertificateView>,
    #[cfg(test)]
    load_balancer_rebuilds: usize,
}

impl SnapshotState {
    fn build(snapshot: GatewaySnapshot) -> Self {
        let mut state = Self {
            load_balancers: HashMap::new(),
            backend_lookup: HashMap::new(),
            backend_ref_counts: HashMap::new(),
            http_routes: HashMap::new(),
            tcp_routes: HashMap::new(),
            http_by_host: HashMap::new(),
            http_wildcard: Vec::new(),
            acme_challenges: snapshot.acme_challenges.clone(),
            certificates: snapshot.certificates.clone(),
            snapshot: Arc::new(GatewaySnapshot::empty()),
            #[cfg(test)]
            load_balancer_rebuilds: 0,
        };

        for route in &snapshot.http_routes {
            state.insert_http_route(Arc::new(route.clone()));
        }
        for route in &snapshot.tcp_routes {
            state
                .tcp_routes
                .insert(route.route_id.clone(), Arc::new(route.clone()));
        }
        state.snapshot = Arc::new(snapshot);
        state
    }

    fn apply_deltas(&self, deltas: &[ProjectionDelta]) -> Self {
        let mut next = Self {
            snapshot: Arc::clone(&self.snapshot),
            load_balancers: self.load_balancers.clone(),
            backend_lookup: self.backend_lookup.clone(),
            backend_ref_counts: self.backend_ref_counts.clone(),
            http_routes: self.http_routes.clone(),
            tcp_routes: self.tcp_routes.clone(),
            http_by_host: self.http_by_host.clone(),
            http_wildcard: self.http_wildcard.clone(),
            acme_challenges: self.acme_challenges.clone(),
            certificates: self.certificates.clone(),
            #[cfg(test)]
            load_balancer_rebuilds: self.load_balancer_rebuilds,
        };

        for delta in deltas {
            match delta {
                ProjectionDelta::RoutesChanged {
                    removed_route_ids,
                    upserted_http,
                    upserted_tcp,
                } => {
                    for route_id in removed_route_ids {
                        next.remove_route(route_id.as_str());
                    }
                    for route in upserted_http {
                        next.insert_http_route(Arc::new(route.clone()));
                    }
                    for route in upserted_tcp {
                        next.tcp_routes
                            .insert(route.route_id.clone(), Arc::new(route.clone()));
                    }
                }
                ProjectionDelta::CertificateChanged { hostname, value } => match value {
                    Some(value) => {
                        next.certificates.insert(hostname.clone(), value.clone());
                    }
                    None => {
                        next.certificates.remove(hostname);
                    }
                },
                ProjectionDelta::ChallengeChanged { key, value } => match value {
                    Some(value) => {
                        next.acme_challenges.insert(key.clone(), value.clone());
                    }
                    None => {
                        next.acme_challenges.remove(key);
                    }
                },
                ProjectionDelta::Empty => {}
            }
        }

        next
    }

    fn insert_http_route(&mut self, route: Arc<HttpRouteView>) {
        self.remove_route(&route.route_id);
        self.rebuild_load_balancer(&route);
        if route.hostnames.is_empty() {
            insert_sorted_route(&mut self.http_wildcard, Arc::clone(&route));
        } else {
            for hostname in &route.hostnames {
                insert_sorted_route(
                    self.http_by_host.entry(hostname.clone()).or_default(),
                    Arc::clone(&route),
                );
            }
        }
        self.http_routes.insert(route.route_id.clone(), route);
    }

    fn remove_route(&mut self, route_id: &str) {
        if let Some(route) = self.http_routes.remove(route_id) {
            self.load_balancers.remove(route_id);
            for backend in route.backends.iter() {
                self.decrement_backend_ref(backend.address);
            }
            self.http_wildcard
                .retain(|route| route.route_id.as_str() != route_id);
            for routes in self.http_by_host.values_mut() {
                routes.retain(|route| route.route_id.as_str() != route_id);
            }
            self.http_by_host.retain(|_, routes| !routes.is_empty());
        }
        self.tcp_routes.remove(route_id);
    }

    fn rebuild_load_balancer(&mut self, route: &HttpRouteView) {
        if route.backends.is_empty() {
            return;
        }
        #[cfg(test)]
        {
            self.load_balancer_rebuilds += 1;
        }

        let mut addrs = Vec::with_capacity(route.backends.len());
        for backend in &route.backends {
            addrs.push(backend.address.to_string());
            self.backend_lookup.insert(backend.address, backend.clone());
            *self.backend_ref_counts.entry(backend.address).or_default() += 1;
        }

        let Ok(lb) = LoadBalancer::try_from_iter(addrs) else {
            return;
        };
        self.load_balancers
            .insert(route.route_id.clone(), Arc::new(lb));
    }

    fn decrement_backend_ref(&mut self, address: SocketAddr) {
        let Some(count) = self.backend_ref_counts.get_mut(&address) else {
            self.backend_lookup.remove(&address);
            return;
        };
        *count -= 1;
        if *count == 0 {
            self.backend_ref_counts.remove(&address);
            self.backend_lookup.remove(&address);
        }
    }

    #[must_use]
    pub fn match_http_route(&self, host: Option<&str>, path: &str) -> Option<Arc<HttpRouteView>> {
        let host = host
            .map(normalize_request_host)
            .filter(|value| !value.is_empty());
        let path = normalize_path_prefix(path);
        if let Some(host) = host.as_deref()
            && let Some(route) = self
                .http_by_host
                .get(host)
                .and_then(|routes| first_path_match(routes, &path))
        {
            return Some(route);
        }
        first_path_match(&self.http_wildcard, &path)
    }

    #[must_use]
    pub fn match_acme_challenge(
        &self,
        host: Option<&str>,
        path: &str,
    ) -> Option<&AcmeChallengeView> {
        let host = host
            .map(normalize_request_host)
            .filter(|value| !value.is_empty())?;
        let token = path.strip_prefix("/.well-known/acme-challenge/")?;
        self.acme_challenges.get(&(host, token.to_string()))
    }

    #[must_use]
    pub fn certificate(&self, server_name: &str) -> Option<&CertificateView> {
        let hostname = normalize_request_host(server_name);
        self.certificates.get(&hostname)
    }

    #[must_use]
    pub fn route_counts(&self) -> (usize, usize) {
        (self.http_routes.len(), self.tcp_routes.len())
    }

    #[cfg(test)]
    #[must_use]
    pub fn load_balancer_rebuilds(&self) -> usize {
        self.load_balancer_rebuilds
    }

    #[must_use]
    pub fn to_view_snapshot(&self) -> GatewaySnapshot {
        let mut http_routes = self
            .http_routes
            .values()
            .map(|route| route.as_ref().clone())
            .collect::<Vec<_>>();
        let mut tcp_routes = self
            .tcp_routes
            .values()
            .map(|route| route.as_ref().clone())
            .collect::<Vec<_>>();
        http_routes.sort_by_key(|route| {
            (
                route.hostnames.is_empty(),
                std::cmp::Reverse(route.path_prefix.len()),
                route.namespace.0.clone(),
                route.service.clone(),
                route.route_id.clone(),
            )
        });
        tcp_routes.sort_by_key(|route| (route.listen_port, route.route_id.clone()));
        GatewaySnapshot {
            http_routes,
            tcp_routes,
            acme_challenges: self.acme_challenges.clone(),
            certificates: self.certificates.clone(),
        }
    }
}

#[derive(Clone)]
pub struct SharedSnapshot {
    inner: Arc<RwLock<Arc<SnapshotState>>>,
}

impl SharedSnapshot {
    #[must_use]
    pub fn new(snapshot: GatewaySnapshot) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(SnapshotState::build(snapshot)))),
        }
    }

    #[must_use]
    pub fn load(&self) -> Arc<SnapshotState> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn replace(&self, snapshot: GatewaySnapshot) {
        *self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Arc::new(SnapshotState::build(snapshot));
    }

    pub fn apply_deltas(&self, deltas: &[ProjectionDelta]) {
        if deltas.is_empty() {
            return;
        }
        let current = self.load();
        *self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Arc::new(current.apply_deltas(deltas));
    }
}

fn insert_sorted_route(routes: &mut Vec<Arc<HttpRouteView>>, route: Arc<HttpRouteView>) {
    routes.push(route);
    routes.sort_by_key(|route| {
        (
            std::cmp::Reverse(route.path_prefix.len()),
            route.namespace.0.clone(),
            route.service.clone(),
            route.route_id.clone(),
        )
    });
}

fn first_path_match(routes: &[Arc<HttpRouteView>], path: &str) -> Option<Arc<HttpRouteView>> {
    routes
        .iter()
        .find(|route| path.starts_with(route.path_prefix.as_str()))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::RouteId;
    use ployz_types::model::{InstanceId, MachineId, MachineTopology};
    use ployz_types::spec::Namespace;
    use std::net::{Ipv4Addr, SocketAddrV4};

    #[test]
    fn route_delta_rebuilds_only_changed_load_balancer_and_keeps_view_snapshot_stable() {
        let shared = SharedSnapshot::new(GatewaySnapshot {
            http_routes: vec![
                route("route-a", "api.example.com", "/a", 8080),
                route("route-b", "api.example.com", "/b", 8081),
            ],
            tcp_routes: Vec::new(),
            acme_challenges: HashMap::new(),
            certificates: HashMap::new(),
        });
        let before = shared.load();
        let route_b_lb = before
            .load_balancers
            .get("route-b")
            .expect("route-b load balancer")
            .clone();
        let snapshot_view = before.snapshot.clone();
        assert_eq!(before.load_balancer_rebuilds(), 2);

        shared.apply_deltas(&[ProjectionDelta::RoutesChanged {
            removed_route_ids: vec![RouteId::from_raw_for_tests("route-a")],
            upserted_http: vec![route("route-a", "api.example.com", "/a", 9090)],
            upserted_tcp: Vec::new(),
        }]);

        let after = shared.load();
        assert_eq!(after.load_balancer_rebuilds(), 3);
        assert!(Arc::ptr_eq(
            &route_b_lb,
            after
                .load_balancers
                .get("route-b")
                .expect("route-b load balancer")
        ));
        assert!(Arc::ptr_eq(&snapshot_view, &after.snapshot));
        let matched = after
            .match_http_route(Some("api.example.com"), "/a/users")
            .expect("route-a should match");
        assert_eq!(matched.backends[0].address.port(), 9090);
    }

    #[test]
    fn route_delta_preserves_backend_lookup_for_shared_addresses() {
        let shared = SharedSnapshot::new(GatewaySnapshot {
            http_routes: vec![
                route("route-a", "api.example.com", "/a", 8080),
                route("route-b", "api.example.com", "/b", 8080),
            ],
            tcp_routes: Vec::new(),
            acme_challenges: HashMap::new(),
            certificates: HashMap::new(),
        });
        let address = SocketAddr::from(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 8080));

        shared.apply_deltas(&[ProjectionDelta::RoutesChanged {
            removed_route_ids: vec![RouteId::from_raw_for_tests("route-a")],
            upserted_http: Vec::new(),
            upserted_tcp: Vec::new(),
        }]);
        assert!(shared.load().backend_lookup.contains_key(&address));

        shared.apply_deltas(&[ProjectionDelta::RoutesChanged {
            removed_route_ids: vec![RouteId::from_raw_for_tests("route-b")],
            upserted_http: Vec::new(),
            upserted_tcp: Vec::new(),
        }]);
        assert!(!shared.load().backend_lookup.contains_key(&address));
    }

    fn route(route_id: &str, hostname: &str, path_prefix: &str, port: u16) -> HttpRouteView {
        HttpRouteView {
            route_id: route_id.into(),
            namespace: Namespace("prod".into()),
            service: route_id.into(),
            revision_hash: "rev-1".into(),
            hostnames: vec![hostname.into()],
            path_prefix: path_prefix.into(),
            backends: vec![BackendView {
                instance_id: InstanceId(format!("inst-{route_id}")),
                machine_id: MachineId("machine-a".into()),
                topology: MachineTopology::local(),
                service_port: "http".into(),
                address: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), port).into(),
            }],
        }
    }
}
