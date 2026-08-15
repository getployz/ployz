use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use ipnet::Ipv4Net;
use ployz_core::{ContainerId, ContainerObservation, LocalMachinePhase, Machine, MachineId};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{ApiClient, Error, Statement, Subscription};
use crate::{
    hosted_dns::Reservation,
    machine::{LocalMachineRecord, LocalMachineStore, StoreError},
};

#[derive(Clone)]
pub struct ReplicatedStore {
    api: ApiClient,
    machine_publication: Arc<tokio::sync::Mutex<()>>,
}

pub(crate) struct MachinePublicationGuard<'a> {
    store: &'a ReplicatedStore,
    _guard: tokio::sync::MutexGuard<'a, ()>,
}

impl MachinePublicationGuard<'_> {
    pub(crate) fn complete_catch_up(
        &self,
        local: &mut LocalMachineStore,
    ) -> Result<bool, StoreError> {
        if local.record().phase != LocalMachinePhase::Joining {
            return Ok(false);
        }
        local.complete_catch_up()?;
        Ok(true)
    }

    pub(crate) fn publishable_machine(&self, local: &LocalMachineRecord) -> Option<Machine> {
        (local.phase == LocalMachinePhase::Participating)
            .then(|| local.machine.clone())
            .flatten()
    }

    pub(crate) async fn publish(&self, machine: &Machine) -> Result<(), Error> {
        self.store.publish_local_machine_unlocked(machine).await
    }

    pub(crate) async fn remove(&self, machine_id: &MachineId) -> Result<(), Error> {
        self.store
            .api
            .execute([Statement::new(
                "DELETE FROM containers WHERE machine_id = ?",
                [json!(machine_id)],
            )])
            .await?;
        self.store
            .api
            .execute([Statement::new(
                "DELETE FROM machines WHERE id = ?",
                [json!(machine_id)],
            )])
            .await
    }

    pub(crate) async fn reconcile_local_containers(
        &self,
        local: &LocalMachineRecord,
        machine_id: &MachineId,
        current: &LocalContainerSnapshot,
    ) -> Result<(), Error> {
        if local.phase != LocalMachinePhase::Participating {
            return Ok(());
        }
        self.store
            .reconcile_local_containers_unlocked(machine_id, current)
            .await
    }
}

impl ReplicatedStore {
    #[must_use]
    pub(crate) fn new(api: ApiClient) -> Self {
        Self {
            api,
            machine_publication: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub(super) fn api(&self) -> &ApiClient {
        &self.api
    }

    pub async fn publish_local_machine(&self, machine: &Machine) -> Result<(), Error> {
        self.machine_publication().await.publish(machine).await
    }

    pub async fn remove_machine(&self, machine_id: &MachineId) -> Result<(), Error> {
        self.machine_publication().await.remove(machine_id).await
    }

    pub(crate) async fn machine_publication(&self) -> MachinePublicationGuard<'_> {
        MachinePublicationGuard {
            store: self,
            _guard: self.machine_publication.lock().await,
        }
    }

    async fn publish_local_machine_unlocked(&self, machine: &Machine) -> Result<(), Error> {
        let current = self.machine(machine.id.as_str()).await?;
        if current.as_ref() == Some(machine) {
            return Ok(());
        }
        self.api
            .execute([Statement::new(
                "INSERT INTO machines (id, info, created_at, updated_at) VALUES (?, ?, datetime('now'), datetime('now')) ON CONFLICT (id) DO UPDATE SET info = excluded.info, updated_at = excluded.updated_at",
                [json!(machine.id), json!(serde_json::to_string(machine)?)],
            )])
            .await?;
        Ok(())
    }

    pub async fn publish_cluster_network(&self, network: Ipv4Net) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO cluster (key, value, updated_at) VALUES ('network', ?, datetime('now')) ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                [json!(network.to_string())],
            )])
            .await?;
        Ok(())
    }

    pub async fn cluster_network(&self) -> Result<Ipv4Net, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT value FROM cluster WHERE key = 'network'",
                [],
            ))
            .await?;
        let rows = query.rows(["value"])?;
        let Some([value]) = rows.first() else {
            return Err(Error::Protocol("Cluster network is missing".into()));
        };
        text(value, "Cluster network")?
            .parse()
            .map_err(|error| Error::Protocol(format!("invalid Cluster network: {error}")))
    }

    pub(crate) async fn domain_reservation(&self) -> Result<Option<Reservation>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT value FROM cluster WHERE key = 'hosted_dns'",
                [],
            ))
            .await?;
        let rows = query.rows(["value"])?;
        let Some([value]) = rows.first() else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(text(
            value,
            "hosted DNS reservation",
        )?)?))
    }

    pub(crate) async fn publish_domain_reservation(
        &self,
        reservation: &Reservation,
    ) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO cluster (key, value, updated_at) VALUES ('hosted_dns', ?, datetime('now')) ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                [json!(serde_json::to_string(reservation)?)],
            )])
            .await
    }

    pub(crate) async fn remove_domain_reservation(&self) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "DELETE FROM cluster WHERE key = 'hosted_dns'",
                [],
            )])
            .await
    }

    pub async fn machine(&self, id: &str) -> Result<Option<Machine>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT info FROM machines WHERE id = ?",
                [json!(id)],
            ))
            .await?;
        let rows = query.rows(["info"])?;
        let Some([info]) = rows.first() else {
            return Ok(None);
        };
        let info = text(info, "machine info")?;
        if info.is_empty() || info == "{}" {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(info)?))
    }

    pub async fn machines(&self) -> Result<ReplicatedObservations<Machine>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, info FROM machines ORDER BY name",
                [],
            ))
            .await?;
        decode_observations(query.rows(["id", "info"])?)
    }

    pub async fn publish_container(&self, observation: &ContainerObservation) -> Result<(), Error> {
        let observation = redacted_container(observation);
        if self.container(&observation.container_id).await? == Some(observation.clone()) {
            return Ok(());
        }
        self.api.execute([container_upsert(&observation)?]).await?;
        Ok(())
    }

    pub async fn container(&self, id: &ContainerId) -> Result<Option<ContainerObservation>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT container FROM containers WHERE id = ?",
                [json!(id)],
            ))
            .await?;
        let rows = query.rows(["container"])?;
        let Some([encoded]) = rows.first() else {
            return Ok(None);
        };
        let encoded = text(encoded, "replicated container JSON")?;
        if encoded.is_empty() || encoded == "{}" {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(encoded)?))
    }

    #[cfg(test)]
    pub(crate) async fn raw_container(&self, id: &ContainerId) -> Result<Option<String>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT container FROM containers WHERE id = ?",
                [json!(id)],
            ))
            .await?;
        query
            .rows(["container"])?
            .first()
            .map(|[encoded]| text(encoded, "replicated container JSON").map(ToOwned::to_owned))
            .transpose()
    }

    async fn local_containers(
        &self,
        machine_id: &MachineId,
    ) -> Result<LocalContainerSnapshot, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, container FROM containers WHERE machine_id = ? ORDER BY id",
                [json!(machine_id)],
            ))
            .await?;
        let mut snapshot = LocalContainerSnapshot::default();
        for [id, encoded] in query.rows(["id", "container"])? {
            let id = ContainerId::parse(text(&id, "container ID")?.to_owned())?;
            let encoded = text(&encoded, "replicated container JSON")?;
            let observation = if encoded.is_empty() || encoded == "{}" {
                None
            } else {
                Some(serde_json::from_str(encoded)?)
            };
            snapshot.inventory.insert(id.clone());
            if let Some(observation) = observation {
                snapshot.observations.insert(id, observation);
            }
        }
        Ok(snapshot)
    }

    async fn reconcile_local_containers_unlocked(
        &self,
        machine_id: &MachineId,
        current: &LocalContainerSnapshot,
    ) -> Result<(), Error> {
        if current
            .observations
            .values()
            .any(|observation| &observation.machine_id != machine_id)
        {
            return Err(Error::Protocol(
                "local container reconciliation crossed Machine authority".into(),
            ));
        }
        let existing = self.local_containers(machine_id).await?;
        let current = LocalContainerSnapshot {
            inventory: current.inventory.clone(),
            observations: current
                .observations
                .iter()
                .map(|(id, observation)| (id.clone(), redacted_container(observation)))
                .collect(),
        };
        let changes = local_container_changes(&existing, &current);
        let mut statements = changes
            .deletions
            .iter()
            .map(|id| {
                Statement::new(
                    "DELETE FROM containers WHERE id = ? AND machine_id = ?",
                    [json!(id), json!(machine_id)],
                )
            })
            .collect::<Vec<_>>();
        for observation in &changes.upserts {
            statements.push(container_upsert(observation)?);
        }
        if !statements.is_empty() {
            self.api.execute(statements).await?;
        }
        Ok(())
    }

    pub async fn containers(&self) -> Result<ReplicatedObservations<ContainerObservation>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, container FROM containers ORDER BY id",
                [],
            ))
            .await?;
        decode_observations(query.rows(["id", "container"])?)
    }

    pub(crate) async fn subscribe_container_changes(&self) -> Result<Subscription, Error> {
        self.api
            .subscribe(Statement::new("SELECT id, container FROM containers", []))
            .await
    }

    pub async fn version(&self) -> Result<BTreeMap<String, i64>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT site_id, db_version FROM crsql_db_versions",
                [],
            ))
            .await?;
        query
            .rows(["site_id", "db_version"])?
            .into_iter()
            .map(|[actor, version]| {
                let actor = actor_id(&actor)?;
                let version = version
                    .as_i64()
                    .ok_or_else(|| Error::Protocol("invalid actor version".into()))?;
                Ok((actor, version))
            })
            .collect()
    }

    pub async fn has_known_missing_changes(&self) -> Result<bool, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT EXISTS (SELECT 1 FROM __corro_bookkeeping_gaps) AS has_gaps",
                [],
            ))
            .await?;
        match query.rows(["has_gaps"])?.as_slice() {
            [[Value::Number(value)]] => value
                .as_u64()
                .filter(|value| *value <= 1)
                .map(|value| value == 1)
                .ok_or_else(|| Error::Protocol("invalid gap status".into())),
            _ => Err(Error::Protocol("invalid gap status row".into())),
        }
    }
}

#[derive(Debug, PartialEq)]
struct LocalContainerChanges {
    deletions: Vec<ContainerId>,
    upserts: Vec<ContainerObservation>,
}

#[derive(Default)]
pub(crate) struct LocalContainerSnapshot {
    inventory: BTreeSet<ContainerId>,
    observations: BTreeMap<ContainerId, ContainerObservation>,
}

impl LocalContainerSnapshot {
    pub(crate) fn from_inventory(ids: Vec<ContainerId>) -> Self {
        Self {
            inventory: ids.into_iter().collect(),
            observations: BTreeMap::new(),
        }
    }

    pub(crate) fn ids(&self) -> impl Iterator<Item = &ContainerId> {
        self.inventory.iter()
    }

    pub(crate) fn observed(&mut self, observation: ContainerObservation) {
        debug_assert!(self.inventory.contains(&observation.container_id));
        self.observations
            .insert(observation.container_id.clone(), observation);
    }
}

fn local_container_changes(
    existing: &LocalContainerSnapshot,
    current: &LocalContainerSnapshot,
) -> LocalContainerChanges {
    LocalContainerChanges {
        deletions: existing
            .inventory
            .iter()
            .filter(|id| !current.inventory.contains(id))
            .cloned()
            .collect(),
        upserts: current
            .observations
            .values()
            .filter(|observation| {
                existing.observations.get(&observation.container_id) != Some(observation)
            })
            .cloned()
            .collect(),
    }
}

fn container_upsert(observation: &ContainerObservation) -> Result<Statement, Error> {
    Ok(Statement::new(
        "INSERT INTO containers (id, container, machine_id, updated_at) VALUES (?, ?, ?, datetime('now')) ON CONFLICT (id) DO UPDATE SET container = excluded.container, machine_id = excluded.machine_id, updated_at = excluded.updated_at",
        [
            json!(observation.container_id),
            json!(serde_json::to_string(observation)?),
            json!(observation.machine_id),
        ],
    ))
}

fn redacted_container(observation: &ContainerObservation) -> ContainerObservation {
    let mut observation = observation.clone();
    observation.resolved_spec.container.environment.clear();
    if let Some(hook) = &mut observation.resolved_spec.pre_deploy {
        hook.environment.clear();
    }
    observation
}

#[derive(Debug, Eq, PartialEq)]
pub struct ReplicatedObservations<T> {
    pub observations: Vec<T>,
    pub incomplete_ids: Vec<String>,
}

pub async fn wait_for_catch_up(
    store: &ReplicatedStore,
    target: &BTreeMap<String, i64>,
) -> Result<(), Error> {
    if target.is_empty() {
        return Ok(());
    }
    let warning_interval = Duration::from_secs(5 * 60);
    let mut warning_at = tokio::time::Instant::now() + warning_interval;
    loop {
        let status = match store.version().await {
            Ok(local) => {
                let lagging = target
                    .iter()
                    .filter(|(actor, target)| {
                        local.get(*actor).copied().unwrap_or_default() < **target
                    })
                    .count();
                if lagging == 0 {
                    match store.has_known_missing_changes().await {
                        Ok(false) => return Ok(()),
                        Ok(true) => "known bookkeeping gaps remain".to_owned(),
                        Err(error) => error.to_string(),
                    }
                } else {
                    format!("{lagging} actor(s) remain behind the target")
                }
            }
            Err(error) => error.to_string(),
        };
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
            () = tokio::time::sleep_until(warning_at) => {
                eprintln!("cluster store catch-up is still pending: {status}");
                warning_at = tokio::time::Instant::now() + warning_interval;
            }
        }
    }
}

pub async fn run_machine_publisher(
    replicated: Option<ReplicatedStore>,
    local: Arc<Mutex<LocalMachineStore>>,
    participating: watch::Sender<bool>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let (restart, _) = watch::channel(false);
    run_machine_publisher_with_restart(replicated, local, participating, restart, shutdown).await
}

pub async fn run_machine_publisher_with_restart(
    replicated: Option<ReplicatedStore>,
    local: Arc<Mutex<LocalMachineStore>>,
    participating: watch::Sender<bool>,
    restart: watch::Sender<bool>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    if let Some(replicated) = &replicated {
        let (joining, target) = {
            let local = local
                .lock()
                .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
            (
                local.record().phase == LocalMachinePhase::Joining,
                local.record().min_store_version.clone(),
            )
        };
        if joining {
            tokio::select! {
                result = wait_for_catch_up(replicated, &target) => {
                    result.map_err(io::Error::other)?;
                }
                () = shutdown.cancelled() => {
                    return Ok(());
                }
            }
            let publication = replicated.machine_publication().await;
            let completed = {
                let mut local = local
                    .lock()
                    .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
                publication
                    .complete_catch_up(&mut local)
                    .map_err(io::Error::other)?
            };
            if completed {
                participating.send_replace(true);
                restart.send_replace(true);
            }
        }
    }
    loop {
        if let Some(replicated) = &replicated {
            let cluster_network = {
                let local = local
                    .lock()
                    .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
                local.record().cluster_network
            };
            if let Some(network) = cluster_network
                && let Err(error) = replicated.publish_cluster_network(network).await
            {
                eprintln!("failed to publish Cluster network: {error}");
            }
            let publication = replicated.machine_publication().await;
            let machine = {
                let local = local
                    .lock()
                    .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
                publication.publishable_machine(local.record())
            };
            if let Some(machine) = machine
                && let Err(error) = publication.publish(&machine).await
            {
                eprintln!("failed to publish local Machine: {error}");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
            () = shutdown.cancelled() => {
                return Ok(());
            }
        }
    }
}

fn decode_observations<T: DeserializeOwned>(
    rows: Vec<[Value; 2]>,
) -> Result<ReplicatedObservations<T>, Error> {
    let mut observations = Vec::new();
    let mut incomplete_ids = Vec::new();
    for [id, encoded] in rows {
        let id = text(&id, "row ID")?.to_owned();
        let encoded = text(&encoded, "replicated JSON")?;
        if encoded.is_empty() || encoded == "{}" {
            incomplete_ids.push(id);
        } else {
            observations.push(serde_json::from_str(encoded)?);
        }
    }
    Ok(ReplicatedObservations {
        observations,
        incomplete_ids,
    })
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, Error> {
    value
        .as_str()
        .ok_or_else(|| Error::Protocol(format!("invalid {field}")))
}

fn actor_id(value: &Value) -> Result<String, Error> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Array(bytes) => bytes
            .iter()
            .map(|byte| {
                byte.as_u64()
                    .and_then(|byte| u8::try_from(byte).ok())
                    .map(|byte| format!("{byte:02x}"))
                    .ok_or_else(|| Error::Protocol("invalid actor ID byte".into()))
            })
            .collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {
            Err(Error::Protocol("invalid actor ID".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        net::TcpListener,
        sync::{Arc, Mutex},
    };

    use ployz_core::{ContainerId, ContainerObservation, LocalMachinePhase, Machine};
    use serde_json::json;

    use super::{
        LocalContainerSnapshot, ReplicatedStore, local_container_changes, redacted_container,
    };
    use crate::corrosion::ApiClient;
    use crate::machine::{LocalMachineRecord, LocalMachineStore};

    #[tokio::test]
    async fn catch_up_waits_for_removal_and_rechecks_phase() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let store = ReplicatedStore::new(
            ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
        );
        let data_dir = std::env::temp_dir().join(format!(
            "ployzd-catch-up-reset-{}",
            ployz_core::MachineId::random()
        ));
        let mut local = LocalMachineStore::open(&data_dir).unwrap();
        let public_key = local
            .record()
            .wireguard_private_key
            .as_ref()
            .unwrap()
            .public_key();
        let machine: Machine = serde_json::from_value(json!({
            "id": "b".repeat(32),
            "name": "joining",
            "subnet": "10.210.1.0/24",
            "management_address": "fdcc::1",
            "public_key": public_key.0,
        }))
        .unwrap();
        local
            .join(machine.clone(), vec![machine], BTreeMap::new(), None)
            .unwrap();
        let local = Arc::new(Mutex::new(local));

        let first = store.machine_publication().await;
        let clone = store.clone();
        let task_local = Arc::clone(&local);
        let (started, waiting) = tokio::sync::oneshot::channel();
        let second = tokio::spawn(async move {
            started.send(()).unwrap();
            let publication = clone.machine_publication().await;
            publication.complete_catch_up(&mut task_local.lock().unwrap())
        });
        waiting.await.unwrap();
        tokio::task::yield_now().await;
        assert!(!second.is_finished());

        local.lock().unwrap().begin_reset().unwrap();
        drop(first);
        let completed = tokio::time::timeout(std::time::Duration::from_secs(1), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!completed);
        drop(local);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn container_publication_waits_for_removal_and_rechecks_phase() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let store = ReplicatedStore::new(
            ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
        );
        let (machine, local) = participating_record();
        let machine_id = machine.id;
        let local = Arc::new(Mutex::new(local));
        let first = store.machine_publication().await;
        let clone = store.clone();
        let task_local = Arc::clone(&local);
        let (started, waiting) = tokio::sync::oneshot::channel();
        let second = tokio::spawn(async move {
            started.send(()).unwrap();
            let publication = clone.machine_publication().await;
            let local = task_local.lock().unwrap().clone();
            publication
                .reconcile_local_containers(&local, &machine_id, &LocalContainerSnapshot::default())
                .await
        });
        waiting.await.unwrap();
        tokio::task::yield_now().await;
        assert!(!second.is_finished());

        local.lock().unwrap().phase = LocalMachinePhase::Resetting;
        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(1), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn publication_guard_rechecks_the_local_phase() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let store = ReplicatedStore::new(
            ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
        );
        let (machine, mut local) = participating_record();
        let publication = store.machine_publication().await;
        assert_eq!(publication.publishable_machine(&local), Some(machine));

        local.phase = LocalMachinePhase::Resetting;
        assert_eq!(publication.publishable_machine(&local), None);
    }

    fn participating_record() -> (Machine, LocalMachineRecord) {
        let machine: Machine = serde_json::from_value(json!({
            "id": "b".repeat(32),
            "name": "machine",
            "subnet": "10.210.1.0/24",
            "management_address": "fdcc::1",
            "public_key": vec![3; 32],
        }))
        .unwrap();
        let local = LocalMachineRecord {
            id: machine.id.clone(),
            phase: LocalMachinePhase::Participating,
            machine: Some(machine.clone()),
            wireguard_private_key: None,
            wireguard_mtu: None,
            cluster_network: None,
            bootstrap_machines: Vec::new(),
            selected_endpoints: BTreeMap::new(),
            min_store_version: BTreeMap::new(),
        };
        (machine, local)
    }

    #[test]
    fn publication_redacts_service_and_hook_environment_values() {
        let observation: ContainerObservation = serde_json::from_value(json!({
            "container_id": "a".repeat(64),
            "display_name": "api-test",
            "machine_id": "b".repeat(32),
            "service_id": "c".repeat(32),
            "service_name": "api",
            "kind": "service_container",
            "runtime": { "state": "created" },
            "resolved_spec": {
                "service_id": "c".repeat(32),
                "name": "api",
                "mode": { "mode": "replicated", "replicas": 1 },
                "container": {
                    "image": "alpine:3.23.3",
                    "environment": { "TOKEN": "service-secret" },
                    "pull_policy": "missing"
                },
                "pre_deploy": {
                    "command": ["true"],
                    "environment": { "TOKEN": "hook-secret" }
                }
            }
        }))
        .unwrap();

        let redacted = redacted_container(&observation);
        assert!(redacted.resolved_spec.container.environment.is_empty());
        assert!(
            redacted
                .resolved_spec
                .pre_deploy
                .unwrap()
                .environment
                .is_empty()
        );
        assert_eq!(
            observation
                .resolved_spec
                .container
                .environment
                .get("TOKEN")
                .map(String::as_str),
            Some("service-secret")
        );
    }

    #[test]
    fn snapshot_diff_upserts_changes_and_deletes_only_absent_ids() {
        let observation: ContainerObservation = serde_json::from_value(json!({
            "container_id": "a".repeat(64),
            "display_name": "api-test",
            "machine_id": "b".repeat(32),
            "service_id": "c".repeat(32),
            "service_name": "api",
            "kind": "service_container",
            "runtime": { "state": "created" },
            "resolved_spec": {
                "service_id": "c".repeat(32),
                "name": "api",
                "mode": { "mode": "replicated", "replicas": 1 },
                "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
            }
        }))
        .unwrap();
        let stable = observation.clone();
        let mut stale = observation.clone();
        stale.container_id = ContainerId::parse("b".repeat(64)).unwrap();
        let mut old_changed = observation.clone();
        old_changed.container_id = ContainerId::parse("c".repeat(64)).unwrap();
        let mut changed = old_changed.clone();
        changed.display_name = "renamed".into();
        let mut new = observation.clone();
        new.container_id = ContainerId::parse("d".repeat(64)).unwrap();

        let existing = LocalContainerSnapshot {
            inventory: [stable.clone(), stale.clone(), old_changed.clone()]
                .into_iter()
                .map(|item| item.container_id.clone())
                .collect(),
            observations: [stable.clone(), stale.clone(), old_changed]
                .into_iter()
                .map(|item| (item.container_id.clone(), item))
                .collect::<BTreeMap<_, _>>(),
        };
        let current = LocalContainerSnapshot {
            inventory: [stable.clone(), changed.clone(), new.clone()]
                .into_iter()
                .map(|item| item.container_id.clone())
                .collect(),
            observations: [stable, changed.clone(), new.clone()]
                .into_iter()
                .map(|item| (item.container_id.clone(), item))
                .collect::<BTreeMap<_, _>>(),
        };
        let changes = local_container_changes(&existing, &current);

        assert_eq!(changes.deletions, vec![stale.container_id]);
        assert_eq!(changes.upserts, vec![changed, new]);
    }
}
