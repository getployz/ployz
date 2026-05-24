//! Composition layer that wires Ployz product ports to adapters.
//!
//! Feature modules should depend on Ployz-owned traits. This module is allowed
//! to assemble concrete adapters and pass them into product orchestration.

use std::{sync::mpsc, thread, time::Duration};

use crate::acme::{
    CertificateAuthorityPort, CertificatePort, CertificateReadinessService, CertificateStatusPort,
};
use crate::adapters::polis::{
    AcmeCertificateIssuer, CorrosionMachineMembership, MachineMembershipRows, PolisDomainStatus,
    PolisMachineMembership, PolisServingSnapshots,
};
use crate::domain::DomainStatusPort;
use crate::error::PrimitiveFailure;
use crate::machine::{MachineMembershipPort, MachineWireGuardPeerPort};
use crate::operation::ScopeId;
use crate::serving::ServingSnapshotPort;
use polis::external_attempt as attempt;

#[must_use]
pub fn certificate_readiness_with_attempts<S, A, I>(
    certificates: S,
    attempts: A,
    issuer: I,
) -> impl CertificatePort
where
    S: CertificateStatusPort,
    A: attempt::Backend,
    I: CertificateAuthorityPort,
{
    CertificateReadinessService::new(certificates, AcmeCertificateIssuer::new(attempts, issuer))
}

#[must_use]
pub fn in_memory_machine_membership() -> impl MachineMembershipPort + MachineWireGuardPeerPort {
    PolisMachineMembership::in_memory()
}

pub fn corrosion_machine_membership<P>(
    store: polis::CorrosionStore,
    probe: P,
    island: polis::IslandId,
) -> Result<impl MachineMembershipPort + MachineWireGuardPeerPort, PrimitiveFailure>
where
    P: polis::PeerProbe,
{
    CorrosionMachineMembershipRows::start(store)
        .map(|rows| CorrosionMachineMembership::new(rows, probe, island))
}

pub fn iroh_peer_rpc_probe(
    endpoint: polis::PeerEndpoint,
    ticket: &polis::PeerTicket,
) -> Result<impl polis::PeerProbe, PrimitiveFailure> {
    polis::PeerRpcProbe::connect(endpoint, ticket).map_err(map_peer_probe_error)
}

fn map_peer_probe_error(error: polis::PeerError) -> PrimitiveFailure {
    match error {
        polis::PeerError::MalformedTicket
        | polis::PeerError::MalformedIdentity
        | polis::PeerError::IdentityIo { .. } => PrimitiveFailure::MalformedPayload,
        polis::PeerError::RpcTimeout => PrimitiveFailure::Timeout,
        polis::PeerError::ProbeFailed { .. }
        | polis::PeerError::RpcTransport { .. }
        | polis::PeerError::RpcRuntime { .. }
        | polis::PeerError::RemoteServiceCannotListen => PrimitiveFailure::NoResponder,
    }
}

enum CorrosionMembershipCommand {
    Observe {
        machine_id: polis::StoreMachineId,
        reply: mpsc::Sender<Result<Option<polis::MachineRow>, polis::StoreError>>,
    },
    Upsert {
        row: polis::MachineRow,
        reply: mpsc::Sender<Result<(), polis::StoreError>>,
    },
    WireGuardPeers {
        machine_id: polis::StoreMachineId,
        reply: mpsc::Sender<Result<Vec<polis::MachineRow>, polis::StoreError>>,
    },
}

struct CorrosionMachineMembershipRows {
    commands: mpsc::Sender<CorrosionMembershipCommand>,
}

impl CorrosionMachineMembershipRows {
    fn start(store: polis::CorrosionStore) -> Result<Self, PrimitiveFailure> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| PrimitiveFailure::OperationInterrupted)?;
        let schema = polis::membership_schema_statements().map_err(map_membership_store_error)?;
        runtime
            .block_on(store.apply_schema(&schema, polis::StoreTimeout::CONTROL_PLANE_DEFAULT))
            .map_err(map_membership_store_error)?;
        let (commands, receiver) = mpsc::channel();

        thread::Builder::new()
            .name("ployz-corrosion-membership".to_string())
            .spawn(move || run_corrosion_membership_worker(store, runtime, receiver))
            .map_err(|_| PrimitiveFailure::OperationInterrupted)?;

        Ok(Self { commands })
    }

    fn worker_stopped() -> polis::StoreError {
        polis::StoreError::Stream {
            message: "corrosion membership worker stopped".to_string(),
        }
    }

    fn reply_timeout() -> Duration {
        Duration::from_secs(polis::StoreTimeout::CONTROL_PLANE_DEFAULT.seconds_value() + 1)
    }
}

fn run_corrosion_membership_worker(
    store: polis::CorrosionStore,
    runtime: tokio::runtime::Runtime,
    receiver: mpsc::Receiver<CorrosionMembershipCommand>,
) {
    let timeout = polis::StoreTimeout::CONTROL_PLANE_DEFAULT;
    while let Ok(command) = receiver.recv() {
        match command {
            CorrosionMembershipCommand::Observe { machine_id, reply } => {
                let result = runtime.block_on(observe_machine_row(&store, &machine_id, timeout));
                let _ = reply.send(result);
            }
            CorrosionMembershipCommand::Upsert { row, reply } => {
                let result = runtime.block_on(upsert_machine_row(&store, &row, timeout));
                let _ = reply.send(result);
            }
            CorrosionMembershipCommand::WireGuardPeers { machine_id, reply } => {
                let result = runtime.block_on(wireguard_peer_rows(&store, &machine_id, timeout));
                let _ = reply.send(result);
            }
        }
    }
}

async fn observe_machine_row(
    store: &polis::CorrosionStore,
    machine_id: &polis::StoreMachineId,
    timeout: polis::StoreTimeout,
) -> Result<Option<polis::MachineRow>, polis::StoreError> {
    let statement = polis::select_machine_statement(machine_id)?;
    let rows = store.query(&statement, timeout).await?;
    let Some(row) = rows.rows().first() else {
        return Ok(None);
    };
    polis::machine_row_from_store_row(row).map(Some)
}

async fn upsert_machine_row(
    store: &polis::CorrosionStore,
    row: &polis::MachineRow,
    timeout: polis::StoreTimeout,
) -> Result<(), polis::StoreError> {
    let statement = polis::upsert_machine_statement(row)?;
    store.execute_transaction(&[statement], timeout).await?;
    Ok(())
}

async fn wireguard_peer_rows(
    store: &polis::CorrosionStore,
    machine_id: &polis::StoreMachineId,
    timeout: polis::StoreTimeout,
) -> Result<Vec<polis::MachineRow>, polis::StoreError> {
    let statement = polis::wireguard_peers_for_machine_statement(machine_id)?;
    let rows = store.query(&statement, timeout).await?;
    rows.rows()
        .iter()
        .map(|row| polis::machine_row_from_store_row(row))
        .collect()
}

impl MachineMembershipRows for CorrosionMachineMembershipRows {
    fn observe_machine(
        &self,
        machine_id: &polis::StoreMachineId,
    ) -> Result<Option<polis::MachineRow>, polis::StoreError> {
        let (reply, response) = mpsc::channel();
        self.commands
            .send(CorrosionMembershipCommand::Observe {
                machine_id: machine_id.clone(),
                reply,
            })
            .map_err(|_| Self::worker_stopped())?;
        response
            .recv_timeout(Self::reply_timeout())
            .map_err(map_membership_reply_error)?
    }

    fn upsert_machine(&self, row: &polis::MachineRow) -> Result<(), polis::StoreError> {
        let (reply, response) = mpsc::channel();
        self.commands
            .send(CorrosionMembershipCommand::Upsert {
                row: row.clone(),
                reply,
            })
            .map_err(|_| Self::worker_stopped())?;
        response
            .recv_timeout(Self::reply_timeout())
            .map_err(map_membership_reply_error)?
    }

    fn wireguard_peers_for_machine(
        &self,
        machine_id: &polis::StoreMachineId,
    ) -> Result<Vec<polis::MachineRow>, polis::StoreError> {
        let (reply, response) = mpsc::channel();
        self.commands
            .send(CorrosionMembershipCommand::WireGuardPeers {
                machine_id: machine_id.clone(),
                reply,
            })
            .map_err(|_| Self::worker_stopped())?;
        response
            .recv_timeout(Self::reply_timeout())
            .map_err(map_membership_reply_error)?
    }
}

fn map_membership_reply_error(error: mpsc::RecvTimeoutError) -> polis::StoreError {
    match error {
        mpsc::RecvTimeoutError::Timeout => polis::StoreError::Timeout,
        mpsc::RecvTimeoutError::Disconnected => CorrosionMachineMembershipRows::worker_stopped(),
    }
}

fn map_membership_store_error(error: polis::StoreError) -> PrimitiveFailure {
    match error {
        polis::StoreError::MalformedPayload => PrimitiveFailure::MalformedPayload,
        polis::StoreError::Timeout => PrimitiveFailure::Timeout,
        polis::StoreError::Client { .. }
        | polis::StoreError::Response { .. }
        | polis::StoreError::QueryEndedBeforeEndOfQuery
        | polis::StoreError::MissedChange { .. }
        | polis::StoreError::Stream { .. } => PrimitiveFailure::OperationInterrupted,
    }
}

#[must_use]
pub fn in_memory_domain_status(scope: ScopeId) -> impl DomainStatusPort {
    PolisDomainStatus::in_memory(scope)
}

#[must_use]
pub fn in_memory_serving_snapshots(scope: ScopeId) -> impl ServingSnapshotPort {
    PolisServingSnapshots::in_memory(scope)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::acme::{
        CertificateActivation, CertificateDeadline, CertificateIssueRequest,
        CertificateMaterialState, CertificateStatus, CertificateUsability,
        EnsureCertificateOutcome, Hostname, HttpsBinding, RevocationFreshness,
    };
    use crate::error::CertificateFailure;
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, MutationContext, OperationId,
        PrincipalId, ScopeId,
    };

    #[test]
    fn certificate_readiness_composition_uses_attempt_adapter_for_missing_cert() {
        let status = SequenceCertificateStatus::new(vec![
            CertificateStatus::Absent,
            CertificateStatus::Present(usable_certificate()),
        ]);
        let attempts = RecordingAttemptBackend::default();
        let issuer = RecordingCertificateAuthority::default();
        let service = certificate_readiness_with_attempts(status, attempts.clone(), issuer.clone());

        let outcome = service
            .ensure_usable(&context(), &binding(), deadline())
            .expect("ensure certificate");

        assert!(matches!(outcome, EnsureCertificateOutcome::Usable(_)));
        assert_eq!(*issuer.calls.borrow(), 1);
        assert_eq!(
            attempts.terminals.borrow().as_slice(),
            &[attempt::TerminalMarker::Succeeded]
        );
        let records = attempts.records.borrow();
        let Some(record) = records.first() else {
            panic!("expected certificate issuance checkpoint");
        };
        let attempt::EvidenceKind::Checkpoint(payload) = &record.kind else {
            panic!("expected checkpoint evidence");
        };
        assert_eq!(payload, b"certificate-issued");
    }

    #[derive(Clone)]
    struct SequenceCertificateStatus {
        statuses: Rc<RefCell<VecDeque<CertificateStatus>>>,
    }

    impl SequenceCertificateStatus {
        fn new(statuses: Vec<CertificateStatus>) -> Self {
            Self {
                statuses: Rc::new(RefCell::new(statuses.into())),
            }
        }
    }

    impl CertificateStatusPort for SequenceCertificateStatus {
        fn status(&self, _binding: &HttpsBinding) -> Result<CertificateStatus, CertificateFailure> {
            Ok(self
                .statuses
                .borrow_mut()
                .pop_front()
                .unwrap_or(CertificateStatus::Unknown))
        }
    }

    #[derive(Clone, Default)]
    struct RecordingCertificateAuthority {
        calls: Rc<RefCell<usize>>,
    }

    impl CertificateAuthorityPort for RecordingCertificateAuthority {
        fn issue_certificate(
            &self,
            _context: &MutationContext,
            _request: &CertificateIssueRequest,
        ) -> Result<(), CertificateFailure> {
            *self.calls.borrow_mut() += 1;
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingAttemptBackend {
        records: Rc<RefCell<Vec<attempt::Evidence>>>,
        terminals: Rc<RefCell<Vec<attempt::TerminalMarker>>>,
    }

    impl attempt::Backend for RecordingAttemptBackend {
        fn start_or_replay(
            &self,
            _request: &attempt::BackendRequest,
        ) -> polis::Result<attempt::BackendStart> {
            Ok(attempt::BackendStart::Started)
        }

        fn record(
            &self,
            _operation: &attempt::OperationId,
            evidence: attempt::Evidence,
        ) -> polis::Result<()> {
            self.records.borrow_mut().push(evidence);
            Ok(())
        }

        fn close(
            &self,
            _operation: &attempt::OperationId,
            marker: attempt::TerminalMarker,
        ) -> polis::Result<()> {
            self.terminals.borrow_mut().push(marker);
            Ok(())
        }
    }

    fn binding() -> HttpsBinding {
        HttpsBinding::new(Hostname::parse("app.example.com").expect("hostname"))
    }

    fn deadline() -> CertificateDeadline {
        CertificateDeadline {
            expires_at: UNIX_EPOCH + Duration::from_secs(3_600),
        }
    }

    fn usable_certificate() -> CertificateUsability {
        CertificateUsability {
            hostname: binding().hostname,
            not_after: UNIX_EPOCH + Duration::from_secs(7_200),
            activation: CertificateActivation::Acknowledged,
            material: CertificateMaterialState::PresentProtected,
            revocation: RevocationFreshness::KnownFresh,
        }
    }

    fn context() -> MutationContext {
        MutationContext::new(
            OperationId::parse("deploy-1").expect("operation"),
            IdempotencyKey::parse("deploy-1").expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            SystemTime::now() + Duration::from_secs(60),
        )
    }
}
