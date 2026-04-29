use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use crate::routes::{
    AcmeChallengeView, BackendView, CertificateView, GatewaySnapshot, HttpRouteView,
    ProjectionDelta, RouteId, TcpRouteView, normalize_path_prefix, normalize_request_host,
};
use pingora::lb::prelude::LoadBalancer;
use pingora::lb::selection::RoundRobin;

pub struct SnapshotState {
    pub(crate) load_balancers: Arc<HashMap<RouteId, Arc<LoadBalancer<RoundRobin>>>>,
    pub(crate) backend_lookup: Arc<HashMap<SocketAddr, BackendView>>,
    route_backend_fingerprints: Arc<HashMap<RouteId, Vec<SocketAddr>>>,
    backend_ref_counts: Arc<HashMap<SocketAddr, usize>>,
    http_routes: Arc<HashMap<RouteId, Arc<HttpRouteView>>>,
    tcp_routes: Arc<HashMap<RouteId, Arc<TcpRouteView>>>,
    http_by_host: Arc<HashMap<String, Vec<Arc<HttpRouteView>>>>,
    http_wildcard: Arc<Vec<Arc<HttpRouteView>>>,
    acme_challenges: Arc<HashMap<(String, String), AcmeChallengeView>>,
    certificates: Arc<HashMap<String, CertificateView>>,
    #[cfg(test)]
    load_balancer_rebuilds: usize,
}

impl SnapshotState {
    fn build(snapshot: GatewaySnapshot) -> Self {
        let mut state = Self {
            load_balancers: Arc::new(HashMap::new()),
            backend_lookup: Arc::new(HashMap::new()),
            route_backend_fingerprints: Arc::new(HashMap::new()),
            backend_ref_counts: Arc::new(HashMap::new()),
            http_routes: Arc::new(HashMap::new()),
            tcp_routes: Arc::new(HashMap::new()),
            http_by_host: Arc::new(HashMap::new()),
            http_wildcard: Arc::new(Vec::new()),
            acme_challenges: Arc::new(snapshot.acme_challenges.clone()),
            certificates: Arc::new(snapshot.certificates.clone()),
            #[cfg(test)]
            load_balancer_rebuilds: 0,
        };

        for route in &snapshot.http_routes {
            state.insert_http_route(Arc::new(route.clone()));
        }
        for route in &snapshot.tcp_routes {
            state
                .tcp_routes_mut()
                .insert(route.route_id.clone(), Arc::new(route.clone()));
        }
        state
    }

    fn apply_deltas(&self, deltas: &[ProjectionDelta]) -> Self {
        let mut next = Self {
            load_balancers: self.load_balancers.clone(),
            backend_lookup: self.backend_lookup.clone(),
            route_backend_fingerprints: self.route_backend_fingerprints.clone(),
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
                    let upserted_route_ids = upserted_http
                        .iter()
                        .map(|route| route.route_id.clone())
                        .chain(upserted_tcp.iter().map(|route| route.route_id.clone()))
                        .collect::<HashSet<_>>();
                    for route_id in removed_route_ids {
                        if !upserted_route_ids.contains(route_id) {
                            next.remove_route(route_id);
                        }
                    }
                    for route in upserted_http {
                        next.insert_http_route(Arc::new(route.clone()));
                    }
                    for route in upserted_tcp {
                        next.tcp_routes_mut()
                            .insert(route.route_id.clone(), Arc::new(route.clone()));
                    }
                }
                ProjectionDelta::CertificateChanged { hostname, value } => match value {
                    Some(value) => {
                        next.certificates_mut()
                            .insert(hostname.clone(), value.clone());
                    }
                    None => {
                        next.certificates_mut().remove(hostname);
                    }
                },
                ProjectionDelta::ChallengeChanged { key, value } => match value {
                    Some(value) => {
                        next.acme_challenges_mut()
                            .insert(key.clone(), value.clone());
                    }
                    None => {
                        next.acme_challenges_mut().remove(key);
                    }
                },
                ProjectionDelta::Empty => {}
            }
        }

        next
    }

    fn load_balancers_mut(&mut self) -> &mut HashMap<RouteId, Arc<LoadBalancer<RoundRobin>>> {
        Arc::make_mut(&mut self.load_balancers)
    }

    fn backend_lookup_mut(&mut self) -> &mut HashMap<SocketAddr, BackendView> {
        Arc::make_mut(&mut self.backend_lookup)
    }

    fn route_backend_fingerprints_mut(&mut self) -> &mut HashMap<RouteId, Vec<SocketAddr>> {
        Arc::make_mut(&mut self.route_backend_fingerprints)
    }

    fn backend_ref_counts_mut(&mut self) -> &mut HashMap<SocketAddr, usize> {
        Arc::make_mut(&mut self.backend_ref_counts)
    }

    fn http_routes_mut(&mut self) -> &mut HashMap<RouteId, Arc<HttpRouteView>> {
        Arc::make_mut(&mut self.http_routes)
    }

    fn tcp_routes_mut(&mut self) -> &mut HashMap<RouteId, Arc<TcpRouteView>> {
        Arc::make_mut(&mut self.tcp_routes)
    }

    fn http_by_host_mut(&mut self) -> &mut HashMap<String, Vec<Arc<HttpRouteView>>> {
        Arc::make_mut(&mut self.http_by_host)
    }

    fn http_wildcard_mut(&mut self) -> &mut Vec<Arc<HttpRouteView>> {
        Arc::make_mut(&mut self.http_wildcard)
    }

    fn acme_challenges_mut(&mut self) -> &mut HashMap<(String, String), AcmeChallengeView> {
        Arc::make_mut(&mut self.acme_challenges)
    }

    fn certificates_mut(&mut self) -> &mut HashMap<String, CertificateView> {
        Arc::make_mut(&mut self.certificates)
    }

    fn insert_http_route(&mut self, route: Arc<HttpRouteView>) {
        let old_route = self.http_routes_mut().remove(&route.route_id);
        if let Some(old_route) = old_route.as_deref() {
            self.remove_http_route_from_indexes(old_route);
        }

        let new_fingerprint = backend_fingerprint(&route.backends);
        let old_fingerprint = self
            .route_backend_fingerprints
            .get(&route.route_id)
            .cloned()
            .unwrap_or_default();
        if old_fingerprint == new_fingerprint {
            for backend in &route.backends {
                self.backend_lookup_mut()
                    .insert(backend.address, backend.clone());
            }
        } else {
            self.load_balancers_mut().remove(&route.route_id);
            if let Some(old_route) = old_route.as_deref() {
                for backend in old_route.backends.iter() {
                    self.decrement_backend_ref(backend.address);
                }
            }
            self.rebuild_load_balancer(&route, new_fingerprint);
        }

        if route.hostnames.is_empty() {
            insert_sorted_route(self.http_wildcard_mut(), Arc::clone(&route));
        } else {
            for hostname in &route.hostnames {
                insert_sorted_route(
                    self.http_by_host_mut().entry(hostname.clone()).or_default(),
                    Arc::clone(&route),
                );
            }
        }
        self.http_routes_mut().insert(route.route_id.clone(), route);
    }

    fn remove_route(&mut self, route_id: &RouteId) {
        if let Some(route) = self.http_routes_mut().remove(route_id) {
            self.load_balancers_mut().remove(route_id);
            self.route_backend_fingerprints_mut().remove(route_id);
            for backend in route.backends.iter() {
                self.decrement_backend_ref(backend.address);
            }
            self.remove_http_route_from_indexes(&route);
        }
        self.tcp_routes_mut().remove(route_id);
    }

    fn remove_http_route_from_indexes(&mut self, route: &HttpRouteView) {
        if route.hostnames.is_empty() {
            self.http_wildcard_mut()
                .retain(|candidate| candidate.route_id != route.route_id);
        } else {
            for hostname in &route.hostnames {
                let should_remove = if let Some(routes) = self.http_by_host_mut().get_mut(hostname)
                {
                    routes.retain(|candidate| candidate.route_id != route.route_id);
                    routes.is_empty()
                } else {
                    false
                };
                if should_remove {
                    self.http_by_host_mut().remove(hostname);
                }
            }
        }
    }

    fn rebuild_load_balancer(&mut self, route: &HttpRouteView, fingerprint: Vec<SocketAddr>) {
        self.route_backend_fingerprints_mut()
            .insert(route.route_id.clone(), fingerprint);
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
            self.backend_lookup_mut()
                .insert(backend.address, backend.clone());
            *self
                .backend_ref_counts_mut()
                .entry(backend.address)
                .or_default() += 1;
        }

        let Ok(lb) = LoadBalancer::try_from_iter(addrs) else {
            return;
        };
        self.load_balancers_mut()
            .insert(route.route_id.clone(), Arc::new(lb));
    }

    fn decrement_backend_ref(&mut self, address: SocketAddr) {
        let remove = match self.backend_ref_counts_mut().get_mut(&address) {
            Some(count) => {
                *count -= 1;
                *count == 0
            }
            None => {
                self.backend_lookup_mut().remove(&address);
                return;
            }
        };
        if remove {
            self.backend_ref_counts_mut().remove(&address);
            self.backend_lookup_mut().remove(&address);
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
            acme_challenges: self.acme_challenges.as_ref().clone(),
            certificates: self.certificates.as_ref().clone(),
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

fn backend_fingerprint(backends: &[BackendView]) -> Vec<SocketAddr> {
    let mut addresses = backends
        .iter()
        .map(|backend| backend.address)
        .collect::<Vec<_>>();
    addresses.sort();
    addresses
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::ServiceKey;
    use ployz_types::model::{InstanceId, MachineId, MachineTopology};
    use ployz_types::spec::Namespace;
    use std::net::{Ipv4Addr, SocketAddrV4};

    #[test]
    fn route_delta_rebuilds_only_changed_load_balancer_and_keeps_view_snapshot_stable() {
        let route_a = route("route-a", "api.example.com", "/a", 8080);
        let route_b = route("route-b", "api.example.com", "/b", 8081);
        let shared = SharedSnapshot::new(GatewaySnapshot {
            http_routes: vec![route_a.clone(), route_b.clone()],
            tcp_routes: Vec::new(),
            acme_challenges: HashMap::new(),
            certificates: HashMap::new(),
        });
        let before = shared.load();
        let route_b_lb = before
            .load_balancers
            .get(&route_b.route_id)
            .expect("route-b load balancer")
            .clone();
        assert_eq!(before.load_balancer_rebuilds(), 2);

        shared.apply_deltas(&[ProjectionDelta::RoutesChanged {
            removed_route_ids: vec![route_a.route_id],
            upserted_http: vec![route("route-a", "api.example.com", "/a", 9090)],
            upserted_tcp: Vec::new(),
        }]);

        let after = shared.load();
        assert_eq!(after.load_balancer_rebuilds(), 3);
        assert!(Arc::ptr_eq(
            &route_b_lb,
            after
                .load_balancers
                .get(&route_b.route_id)
                .expect("route-b load balancer")
        ));
        let matched = after
            .match_http_route(Some("api.example.com"), "/a/users")
            .expect("route-a should match");
        assert_eq!(matched.backends[0].address.port(), 9090);
    }

    #[test]
    fn route_delta_preserves_backend_lookup_for_shared_addresses() {
        let route_a = route("route-a", "api.example.com", "/a", 8080);
        let route_b = route("route-b", "api.example.com", "/b", 8080);
        let shared = SharedSnapshot::new(GatewaySnapshot {
            http_routes: vec![route_a.clone(), route_b.clone()],
            tcp_routes: Vec::new(),
            acme_challenges: HashMap::new(),
            certificates: HashMap::new(),
        });
        let address = SocketAddr::from(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 8080));

        shared.apply_deltas(&[ProjectionDelta::RoutesChanged {
            removed_route_ids: vec![route_a.route_id],
            upserted_http: Vec::new(),
            upserted_tcp: Vec::new(),
        }]);
        assert!(shared.load().backend_lookup.contains_key(&address));

        shared.apply_deltas(&[ProjectionDelta::RoutesChanged {
            removed_route_ids: vec![route_b.route_id],
            upserted_http: Vec::new(),
            upserted_tcp: Vec::new(),
        }]);
        assert!(!shared.load().backend_lookup.contains_key(&address));
    }

    #[test]
    fn unchanged_backend_delta_does_not_rebuild_load_balancer_in_large_runtime() {
        let routes = (0..1_000)
            .map(|index| {
                route(
                    &format!("route-{index}"),
                    &format!("svc-{index}.example.com"),
                    "/",
                    10_000 + index,
                )
            })
            .collect::<Vec<_>>();
        let target = routes[421].clone();
        let shared = SharedSnapshot::new(GatewaySnapshot {
            http_routes: routes,
            tcp_routes: Vec::new(),
            acme_challenges: HashMap::new(),
            certificates: HashMap::new(),
        });
        let before = shared.load();
        let before_rebuilds = before.load_balancer_rebuilds();
        let target_lb = before
            .load_balancers
            .get(&target.route_id)
            .expect("target load balancer")
            .clone();

        shared.apply_deltas(&[ProjectionDelta::RoutesChanged {
            removed_route_ids: vec![target.route_id.clone()],
            upserted_http: vec![target.clone()],
            upserted_tcp: Vec::new(),
        }]);

        let after = shared.load();
        assert_eq!(after.load_balancer_rebuilds(), before_rebuilds);
        assert!(Arc::ptr_eq(
            &target_lb,
            after
                .load_balancers
                .get(&target.route_id)
                .expect("target load balancer")
        ));
        assert_eq!(
            after
                .match_http_route(Some(target.hostnames[0].as_str()), "/")
                .expect("target route")
                .route_id,
            target.route_id
        );
    }

    #[test]
    fn cert_delta_reuses_route_runtime_maps() {
        let routes = (0..1_000)
            .map(|index| {
                route(
                    &format!("route-{index}"),
                    &format!("svc-{index}.example.com"),
                    "/",
                    10_000 + index,
                )
            })
            .collect::<Vec<_>>();
        let shared = SharedSnapshot::new(GatewaySnapshot {
            http_routes: routes,
            tcp_routes: Vec::new(),
            acme_challenges: HashMap::new(),
            certificates: HashMap::new(),
        });
        let before = shared.load();
        let load_balancers = Arc::clone(&before.load_balancers);
        let backend_lookup = Arc::clone(&before.backend_lookup);
        let route_backend_fingerprints = Arc::clone(&before.route_backend_fingerprints);
        let backend_ref_counts = Arc::clone(&before.backend_ref_counts);
        let http_routes = Arc::clone(&before.http_routes);
        let http_by_host = Arc::clone(&before.http_by_host);
        let http_wildcard = Arc::clone(&before.http_wildcard);

        shared.apply_deltas(&[ProjectionDelta::CertificateChanged {
            hostname: "api.example.com".into(),
            value: None,
        }]);

        let after = shared.load();
        assert!(Arc::ptr_eq(&load_balancers, &after.load_balancers));
        assert!(Arc::ptr_eq(&backend_lookup, &after.backend_lookup));
        assert!(Arc::ptr_eq(
            &route_backend_fingerprints,
            &after.route_backend_fingerprints
        ));
        assert!(Arc::ptr_eq(&backend_ref_counts, &after.backend_ref_counts));
        assert!(Arc::ptr_eq(&http_routes, &after.http_routes));
        assert!(Arc::ptr_eq(&http_by_host, &after.http_by_host));
        assert!(Arc::ptr_eq(&http_wildcard, &after.http_wildcard));
    }

    fn route(route_id: &str, hostname: &str, path_prefix: &str, port: u16) -> HttpRouteView {
        let namespace = Namespace("prod".into());
        let service = route_id.to_string();
        HttpRouteView {
            route_id: RouteId::http(&ServiceKey::new(namespace.clone(), service.clone()), 0),
            namespace,
            service,
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
