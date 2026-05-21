use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::FactKeyPattern;
use mvp_bus::{BusActorHandle, BusAuthority, BusSession};
use mvp_p2panda_transport::{PandaNetFactImportOutcome, PandaNetFactNode};
use mvp_projection::{ProjectionActorHandle, SqliteProjectionStore};
use tokio::sync::Mutex;

use super::daemon_control::DaemonControlRuntime;
use super::{
    DaemonOptions, apply_peer_admissions, ensure_remote_bridges_registered,
    handle_node_agent_rpc_requests, is_recoverable_stream_error, publish_admitted_peers,
    publish_self_join, wireguard_endpoint_id,
};
use crate::error::{NodeError, NodeResult};
use crate::node_agent_rpc::RemoteNodeAgentBridgeSet;
use crate::state::LoadedNodeState;
use mvp_mesh::{MeshError, WireGuardActorHandle, WireGuardAppliedSnapshot, plan_full_mesh};

pub(super) struct DaemonRuntime {
    pub(super) state: LoadedNodeState,
    pub(super) options: DaemonOptions,
    pub(super) product_bus: BusActorHandle,
    pub(super) operator_session: BusSession,
    pub(super) node_agent_request_session: BusSession,
    pub(super) fact_node: Arc<Mutex<PandaNetFactNode>>,
    pub(super) writer_session: BusSession,
    pub(super) author: mvp_p2panda_facts::PandaFactAuthor,
    pub(super) authority: BusAuthority,
    pub(super) remote_bridges: Option<RemoteNodeAgentBridgeSet>,
    pub(super) control: Option<DaemonControlRuntime>,
    pub(super) wireguard: WireGuardActorHandle,
}

pub(super) struct DaemonRuntimeReport {
    pub(super) imported_batches: u64,
    pub(super) imported_operations: u64,
    pub(super) wireguard_applied_revision: Option<u64>,
}

impl DaemonRuntime {
    pub(super) async fn run(mut self) -> NodeResult<DaemonRuntimeReport> {
        let started = Instant::now();
        let mut last_advertised = started;
        let mut last_stream_refresh = started;
        let mut imported_batches = 0;
        let mut imported_operations = 0;

        while started.elapsed() < self.options.run_for {
            if last_advertised.elapsed() >= Duration::from_millis(100) {
                self.advertise_membership().await?;
                last_advertised = Instant::now();
            }

            let import_result = self.import_next_batch().await;
            match import_result {
                Ok(Some(outcomes)) => {
                    imported_batches += 1;
                    imported_operations += outcomes
                        .iter()
                        .filter(|outcome| matches!(outcome, PandaNetFactImportOutcome::Imported))
                        .count() as u64;
                    if let Some(control) = &self.control {
                        control.record_import_progress(imported_batches, imported_operations);
                    }
                    self.handle_observed_facts().await?;
                }
                Ok(None) => {
                    self.refresh_idle_stream(&mut last_stream_refresh).await?;
                    self.handle_observed_facts().await?;
                }
                Err(error) if is_recoverable_stream_error(&error) => {
                    self.refresh_stream(&mut last_stream_refresh).await?;
                }
                Err(error) => return Err(NodeError::Transport { source: error }),
            }
        }

        if let Some(control) = self.control {
            control.shutdown().await?;
        }
        Ok(DaemonRuntimeReport {
            imported_batches,
            imported_operations,
            wireguard_applied_revision: self
                .wireguard
                .status()
                .await
                .ok()
                .and_then(|status| status.applied_revision),
        })
    }

    async fn advertise_membership(&self) -> NodeResult<()> {
        {
            let mut fact_node = self.fact_node.lock().await;
            publish_self_join(
                &mut fact_node,
                &self.state,
                &self.writer_session,
                &self.author,
                &wireguard_endpoint_id(&self.state, &self.options.wireguard),
            )
            .await?;
        }
        let current_state = self.current_state()?;
        let mut fact_node = self.fact_node.lock().await;
        publish_admitted_peers(
            &mut fact_node,
            &current_state,
            &self.writer_session,
            &self.author,
        )
        .await
    }

    async fn import_next_batch(
        &self,
    ) -> Result<Option<Vec<PandaNetFactImportOutcome>>, mvp_p2panda_transport::PandaNetTransportError>
    {
        let mut fact_node = self.fact_node.lock().await;
        fact_node
            .import_next_fact_batch_with_idle_timeout(self.options.import_idle)
            .await
    }

    async fn handle_observed_facts(&mut self) -> NodeResult<()> {
        let current_state = self.current_state()?;
        {
            let mut fact_node = self.fact_node.lock().await;
            apply_peer_admissions(
                &mut fact_node,
                &current_state,
                &self.writer_session,
                &self.authority,
            )
            .await?;
        }
        ensure_remote_bridges_registered(
            &self.product_bus,
            &self.operator_session,
            &current_state,
            Arc::clone(&self.fact_node),
            &self.writer_session,
            &mut self.remote_bridges,
        )
        .await?;
        self.apply_wireguard_snapshot(&current_state).await?;
        let mut fact_node = self.fact_node.lock().await;
        let _handled_rpc = handle_node_agent_rpc_requests(
            &mut fact_node,
            &self.writer_session,
            &self.author,
            &self.product_bus,
            &self.node_agent_request_session,
            &current_state,
        )
        .await?;
        Ok(())
    }

    async fn apply_wireguard_snapshot(&self, current_state: &LoadedNodeState) -> NodeResult<()> {
        let facts = {
            let fact_node = self.fact_node.lock().await;
            fact_node.store()
        };
        let projection = ProjectionActorHandle::spawn(
            Arc::new(facts),
            current_state.island(),
            self.writer_session.clone(),
            FactKeyPattern::parse("/facts/>").map_err(|source| NodeError::Bus { source })?,
            SqliteProjectionStore::new(current_state.paths().projection_db.clone()),
            current_state.paths().gateway_snapshot.clone(),
            current_state.paths().dns_snapshot.clone(),
        );
        let report = projection
            .project_once(Duration::from_secs(2))
            .await
            .map_err(|source| NodeError::Projection { source })?;
        let revision = report
            .state
            .nodes
            .values()
            .map(|node| node.epoch)
            .max()
            .unwrap_or(0);
        let plan = match plan_full_mesh(&report.state, &current_state.node_id(), revision) {
            Ok(plan) => plan,
            Err(MeshError::LocalNodeNotLive { node_id }) if node_id == current_state.node_id() => {
                return Ok(());
            }
            Err(source) => return Err(NodeError::Mesh { source }),
        };
        let snapshot = WireGuardAppliedSnapshot::from_plan(current_state.island(), plan);
        self.wireguard
            .apply(snapshot)
            .await
            .map_err(|source| NodeError::Mesh { source })?;
        Ok(())
    }

    async fn refresh_idle_stream(&self, last_stream_refresh: &mut Instant) -> NodeResult<()> {
        if last_stream_refresh.elapsed() < Duration::from_secs(1) {
            return Ok(());
        }
        self.refresh_stream(last_stream_refresh).await
    }

    async fn refresh_stream(&self, last_stream_refresh: &mut Instant) -> NodeResult<()> {
        let mut fact_node = self.fact_node.lock().await;
        fact_node
            .publish_stored_operations()
            .await
            .map_err(|source| NodeError::Transport { source })?;
        fact_node
            .refresh_stream()
            .await
            .map_err(|source| NodeError::Transport { source })?;
        *last_stream_refresh = Instant::now();
        Ok(())
    }

    fn current_state(&self) -> NodeResult<LoadedNodeState> {
        crate::load_node(self.state.paths().state_dir.clone())
    }
}
