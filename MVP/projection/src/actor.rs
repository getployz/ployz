use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kameo::Actor;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::mailbox;
use kameo::message::{Context, Message};
use kameo::reply::DelegatedReply;
use mvp_bus::{BusSession, FactContentHash, FactKeyPattern, FactPayload, IslandId};

use crate::error::{ProjectionError, ProjectionResult};
use crate::model::ProjectionState;
use crate::reducer::reduce_facts;
use crate::snapshot::{SnapshotWriteReport, write_projection_snapshots};
use crate::source::{CandidateStatus, FactCandidate, FactSource, is_reducible_conflict_kind};
use crate::sqlite::SqliteProjectionStore;

const PROJECTION_ACTOR_MAILBOX_CAPACITY: usize = 16;
const PROJECTION_ACTOR_MAILBOX_TIMEOUT: Duration = Duration::from_secs(5);
const PROJECTION_ACTOR_STATUS_REPLY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ProjectionReport {
    pub state: ProjectionState,
    pub sqlite_path: PathBuf,
    pub gateway_snapshot: Option<SnapshotWriteReport>,
    pub dns_snapshot: Option<SnapshotWriteReport>,
    pub duration: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectionActorStatus {
    pub hints_seen: u64,
    pub last_success: Option<ProjectionSuccessStatus>,
    pub last_failure: Option<ProjectionFailureStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionFailureStatus {
    pub kind: ProjectionFailureKind,
    pub message: String,
}

impl ProjectionFailureStatus {
    fn from_error(error: &ProjectionError) -> Self {
        Self {
            kind: ProjectionFailureKind::from_error(error),
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionFailureKind {
    FactSource,
    Io,
    Json,
    Sqlite,
    SnapshotPath,
    ProjectionStore,
    Timeout { operation: &'static str },
    WorkerJoin,
}

impl ProjectionFailureKind {
    fn from_error(error: &ProjectionError) -> Self {
        match error {
            ProjectionError::FactSource(_) => Self::FactSource,
            ProjectionError::Io(_) => Self::Io,
            ProjectionError::Json(_) => Self::Json,
            ProjectionError::Sqlite(_) => Self::Sqlite,
            ProjectionError::SnapshotPath { .. } => Self::SnapshotPath,
            ProjectionError::ProjectionStore { .. } => Self::ProjectionStore,
            ProjectionError::Timeout { operation } => Self::Timeout { operation },
            ProjectionError::WorkerJoin(_) => Self::WorkerJoin,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectionSuccessStatus {
    pub sqlite_path: PathBuf,
    pub gateway_snapshot: Option<SnapshotWriteReport>,
    pub dns_snapshot: Option<SnapshotWriteReport>,
    pub duration: Duration,
    pub node_count: usize,
    pub service_count: usize,
    pub gateway_route_count: usize,
    pub dns_record_count: usize,
    pub acme_http01_challenge_count: usize,
    pub ignored_fact_count: usize,
}

impl ProjectionSuccessStatus {
    fn from_report(report: &ProjectionReport) -> Self {
        Self {
            sqlite_path: report.sqlite_path.clone(),
            gateway_snapshot: report.gateway_snapshot.clone(),
            dns_snapshot: report.dns_snapshot.clone(),
            duration: report.duration,
            node_count: report.state.nodes.len(),
            service_count: report.state.services.len(),
            gateway_route_count: report
                .state
                .gateway
                .as_ref()
                .map_or(0, |gateway| gateway.routes.len()),
            dns_record_count: report.state.dns.as_ref().map_or(0, |dns| dns.records.len()),
            acme_http01_challenge_count: report.state.acme_http01.len(),
            ignored_fact_count: report
                .state
                .statuses
                .iter()
                .map(|status| status.count)
                .sum(),
        }
    }
}

#[derive(Actor)]
struct ProjectionActor {
    source: Arc<dyn FactSource>,
    island: IslandId,
    session: BusSession,
    pattern: FactKeyPattern,
    sqlite: SqliteProjectionStore,
    gateway_snapshot_path: PathBuf,
    dns_snapshot_path: PathBuf,
    projection_inflight: Arc<AtomicBool>,
    status: ProjectionActorStatus,
}

impl ProjectionActor {
    fn new(
        source: Arc<dyn FactSource>,
        island: IslandId,
        session: BusSession,
        pattern: FactKeyPattern,
        sqlite: SqliteProjectionStore,
        gateway_snapshot_path: PathBuf,
        dns_snapshot_path: PathBuf,
    ) -> Self {
        Self {
            source,
            island,
            session,
            pattern,
            sqlite,
            gateway_snapshot_path,
            dns_snapshot_path,
            projection_inflight: Arc::new(AtomicBool::new(false)),
            status: ProjectionActorStatus::default(),
        }
    }
}

#[derive(Clone)]
pub struct ProjectionActorHandle {
    actor: ActorRef<ProjectionActor>,
}

impl ProjectionActorHandle {
    #[must_use]
    pub fn spawn(
        source: Arc<dyn FactSource>,
        island: IslandId,
        session: BusSession,
        pattern: FactKeyPattern,
        sqlite: SqliteProjectionStore,
        gateway_snapshot_path: PathBuf,
        dns_snapshot_path: PathBuf,
    ) -> Self {
        Self {
            actor: ProjectionActor::spawn_with_mailbox(
                ProjectionActor::new(
                    source,
                    island,
                    session,
                    pattern,
                    sqlite,
                    gateway_snapshot_path,
                    dns_snapshot_path,
                ),
                mailbox::bounded(PROJECTION_ACTOR_MAILBOX_CAPACITY),
            ),
        }
    }

    pub async fn project_once(&self, timeout: Duration) -> ProjectionResult<ProjectionReport> {
        let pending = self
            .actor
            .ask(ProjectOnce { timeout })
            .mailbox_timeout(timeout.min(PROJECTION_ACTOR_MAILBOX_TIMEOUT))
            .enqueue()
            .await
            .map_err(map_mailbox_send_error)?;
        pending.await.map_err(map_send_error)
    }

    pub async fn fact_hint(&self) -> ProjectionResult<()> {
        self.actor
            .ask(FactHint)
            .mailbox_timeout(PROJECTION_ACTOR_MAILBOX_TIMEOUT)
            .reply_timeout(PROJECTION_ACTOR_STATUS_REPLY_TIMEOUT)
            .await
            .map_err(map_send_error)
    }

    pub async fn status(&self) -> ProjectionResult<ProjectionActorStatus> {
        self.actor
            .ask(ReadStatus)
            .mailbox_timeout(PROJECTION_ACTOR_MAILBOX_TIMEOUT)
            .reply_timeout(PROJECTION_ACTOR_STATUS_REPLY_TIMEOUT)
            .await
            .map_err(map_send_error)
    }
}

struct ProjectOnce {
    timeout: Duration,
}

impl Message<ProjectOnce> for ProjectionActor {
    type Reply = DelegatedReply<ProjectionResult<ProjectionReport>>;

    async fn handle(
        &mut self,
        message: ProjectOnce,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(permit) = ProjectionWorkerPermit::try_acquire(&self.projection_inflight) else {
            return ctx.spawn(async {
                Err(ProjectionError::Timeout {
                    operation: "project_once_inflight",
                })
            });
        };
        let job = ProjectionJob {
            source: Arc::clone(&self.source),
            island: self.island.clone(),
            session: self.session.clone(),
            pattern: self.pattern.clone(),
            sqlite: self.sqlite.clone(),
            gateway_snapshot_path: self.gateway_snapshot_path.clone(),
            dns_snapshot_path: self.dns_snapshot_path.clone(),
        };
        let actor = ctx.actor_ref().clone();
        ctx.spawn(run_projection_job(job, actor, message.timeout, permit))
    }
}

async fn run_projection_job(
    job: ProjectionJob,
    actor: ActorRef<ProjectionActor>,
    timeout: Duration,
    permit: ProjectionWorkerPermit,
) -> ProjectionResult<ProjectionReport> {
    let started = Instant::now();
    let deadline = started + timeout;
    let prepared = match tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || job.prepare(started, deadline, permit)),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(ProjectionError::WorkerJoin(error.to_string())),
        Err(_) => Err(ProjectionError::Timeout {
            operation: "project_once",
        }),
    };
    let result = match prepared {
        Ok(prepared) => match tokio::task::spawn_blocking(move || prepared.publish()).await {
            Ok(result) => result,
            Err(error) => Err(ProjectionError::WorkerJoin(error.to_string())),
        },
        Err(error) => Err(error),
    };
    let completion = ProjectionCompleted::from_result(&result);
    let _ = actor.tell(completion).send().await;
    result
}

struct ProjectionWorkerPermit {
    running: Arc<AtomicBool>,
}

impl ProjectionWorkerPermit {
    fn try_acquire(running: &Arc<AtomicBool>) -> Option<Self> {
        if running.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(Self {
                running: Arc::clone(running),
            })
        }
    }
}

impl Drop for ProjectionWorkerPermit {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

struct ProjectionCompleted {
    last_success: Option<ProjectionSuccessStatus>,
    last_failure: Option<ProjectionFailureStatus>,
}

impl ProjectionCompleted {
    fn from_result(result: &ProjectionResult<ProjectionReport>) -> Self {
        match result {
            Ok(report) => Self {
                last_success: Some(ProjectionSuccessStatus::from_report(report)),
                last_failure: None,
            },
            Err(error) => Self {
                last_success: None,
                last_failure: Some(ProjectionFailureStatus::from_error(error)),
            },
        }
    }
}

impl Message<ProjectionCompleted> for ProjectionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        message: ProjectionCompleted,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(success) = message.last_success {
            self.status.last_success = Some(success);
            self.status.last_failure = None;
        } else {
            self.status.last_failure = message.last_failure;
        }
    }
}

struct FactHint;

impl Message<FactHint> for ProjectionActor {
    type Reply = ProjectionResult<()>;

    async fn handle(
        &mut self,
        _message: FactHint,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.status.hints_seen += 1;
        Ok(())
    }
}

struct ReadStatus;

impl Message<ReadStatus> for ProjectionActor {
    type Reply = ProjectionResult<ProjectionActorStatus>;

    async fn handle(
        &mut self,
        _message: ReadStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.status.clone())
    }
}

struct ProjectionJob {
    source: Arc<dyn FactSource>,
    island: IslandId,
    session: BusSession,
    pattern: FactKeyPattern,
    sqlite: SqliteProjectionStore,
    gateway_snapshot_path: PathBuf,
    dns_snapshot_path: PathBuf,
}

impl ProjectionJob {
    fn prepare(
        self,
        started: Instant,
        deadline: Instant,
        permit: ProjectionWorkerPermit,
    ) -> ProjectionResult<PreparedProjection> {
        ensure_before(deadline, "list_candidates")?;
        let candidates = self
            .source
            .list_candidates(&self.island, &self.pattern, &self.session)?;
        ensure_before(deadline, "read_payloads")?;
        let payloads = self.read_payloads(&candidates)?;
        ensure_before(deadline, "reduce")?;
        let state = reduce_facts(&self.island, &candidates, &payloads);
        ensure_before(deadline, "publish")?;
        Ok(PreparedProjection {
            state,
            sqlite: self.sqlite,
            gateway_snapshot_path: self.gateway_snapshot_path,
            dns_snapshot_path: self.dns_snapshot_path,
            started,
            deadline,
            permit,
        })
    }

    fn read_payloads(&self, candidates: &[FactCandidate]) -> ProjectionResult<PayloadMap> {
        let mut seen = BTreeSet::new();
        let verified = candidates
            .iter()
            .filter(|candidate| {
                candidate_payload_is_readable(candidate)
                    && candidate.island() == &self.island
                    && seen.insert(candidate.content_hash().clone())
            })
            .cloned()
            .collect::<Vec<_>>();
        Ok(self
            .source
            .read_payloads(&self.island, &verified, &self.session)?)
    }
}

fn candidate_payload_is_readable(candidate: &FactCandidate) -> bool {
    candidate.status() == CandidateStatus::Verified
        || (candidate.status() == CandidateStatus::Conflict
            && is_reducible_conflict_kind(candidate.kind()))
}

struct PreparedProjection {
    state: ProjectionState,
    sqlite: SqliteProjectionStore,
    gateway_snapshot_path: PathBuf,
    dns_snapshot_path: PathBuf,
    started: Instant,
    deadline: Instant,
    permit: ProjectionWorkerPermit,
}

impl PreparedProjection {
    fn publish(self) -> ProjectionResult<ProjectionReport> {
        let _permit = self.permit;
        ensure_before(self.deadline, "sqlite_stage")?;
        let staged_sqlite = self.sqlite.stage_rebuild(&self.state)?;
        ensure_before(self.deadline, "snapshot_write")?;
        let snapshots = write_projection_snapshots(
            &self.state,
            &self.gateway_snapshot_path,
            &self.dns_snapshot_path,
        )?;
        staged_sqlite.promote()?;
        Ok(ProjectionReport {
            state: self.state,
            sqlite_path: self.sqlite.path().to_path_buf(),
            gateway_snapshot: snapshots.gateway,
            dns_snapshot: snapshots.dns,
            duration: self.started.elapsed(),
        })
    }
}

type PayloadMap = std::collections::BTreeMap<FactContentHash, FactPayload>;

fn ensure_before(deadline: Instant, operation: &'static str) -> ProjectionResult<()> {
    if Instant::now() > deadline {
        return Err(ProjectionError::Timeout { operation });
    }
    Ok(())
}

fn map_send_error<M>(error: SendError<M, ProjectionError>) -> ProjectionError {
    match error {
        SendError::HandlerError(error) => error,
        SendError::Timeout(_) => ProjectionError::Timeout {
            operation: "actor_reply",
        },
        error => ProjectionError::WorkerJoin(error.to_string()),
    }
}

fn map_mailbox_send_error(error: SendError) -> ProjectionError {
    match error {
        SendError::Timeout(_) | SendError::MailboxFull(_) => ProjectionError::Timeout {
            operation: "actor_reply",
        },
        error => ProjectionError::WorkerJoin(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    use crate::actor::ProjectionActorHandle;
    use crate::bus_source::BusFactSource;
    use crate::facts::{
        BackendEndpoint, DnsCommitFact, DnsRecordFact, GatewayCommitFact, NodeId, NodeJoinedFact,
        ProjectionFactPayload, RouteCommitFact, RouteId,
    };
    use crate::source::{
        CandidateStatus, FactCandidate, FactKind, FactSource, FactSourceError, FactSourceResult,
    };
    use crate::sqlite::SqliteProjectionStore;
    use mvp_bus::{
        BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, Grant, IslandId,
        PrincipalId, harness::InMemoryBus,
    };

    fn island(value: &str) -> IslandId {
        IslandId::new(value)
    }

    fn principal(value: &str) -> PrincipalId {
        PrincipalId::new(value)
    }

    fn key(value: &str) -> FactKey {
        FactKey::parse(value).expect("fact key parses")
    }

    fn pattern(value: &str) -> FactKeyPattern {
        FactKeyPattern::parse(value).expect("fact pattern parses")
    }

    fn write_payload(
        bus: &InMemoryBus,
        session: &BusSession,
        key: &str,
        payload: ProjectionFactPayload,
    ) {
        bus.write_fact_payload(
            session,
            self::key(key),
            payload.to_fact_bytes().expect("payload serializes"),
        )
        .expect("write fact payload");
    }

    fn seed_projectable_facts() -> (InMemoryBus, BusSession) {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let session = authority.grant_in(
            island("prod"),
            principal("projection"),
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        write_payload(
            &bus,
            &session,
            "/facts/node/node-1/joined/1",
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-1"),
                epoch: 1,
                overlay_ip: "fd00::1".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        write_payload(
            &bus,
            &session,
            "/facts/routes/route-1",
            ProjectionFactPayload::RouteCommit(RouteCommitFact {
                route_commit_id: "route-1".to_string(),
                route_id: RouteId::new("web-http"),
                hostnames: vec!["web.example.com".to_string()],
                backends: vec![BackendEndpoint {
                    node_id: NodeId::new("node-1"),
                    address: "10.0.0.1:8080".to_string(),
                }],
                old_backends_to_drain: Vec::new(),
            }),
        );
        write_payload(
            &bus,
            &session,
            "/facts/gateway/gateway-1",
            ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
                gateway_commit_id: "gateway-1".to_string(),
                route_commit_id: "route-1".to_string(),
                epoch: 1,
            }),
        );
        write_payload(
            &bus,
            &session,
            "/facts/dns/dns-1",
            ProjectionFactPayload::DnsCommit(DnsCommitFact {
                dns_commit_id: "dns-1".to_string(),
                epoch: 1,
                records: vec![DnsRecordFact {
                    name: "web.example.com".to_string(),
                    record_type: "AAAA".to_string(),
                    value: "fd00::1".to_string(),
                    ttl_seconds: 30,
                }],
            }),
        );
        (bus, session)
    }

    fn actor_for(
        source: Arc<dyn FactSource>,
        session: BusSession,
        dir: &tempfile::TempDir,
    ) -> ProjectionActorHandle {
        ProjectionActorHandle::spawn(
            source,
            island("prod"),
            session,
            pattern("/facts/>"),
            SqliteProjectionStore::new(dir.path().join("projections.sqlite")),
            dir.path().join("gateway.snapshot"),
            dir.path().join("dns.snapshot"),
        )
    }

    #[tokio::test]
    async fn project_once_writes_sqlite_and_snapshots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (bus, session) = seed_projectable_facts();
        let actor = actor_for(Arc::new(BusFactSource::new(bus)), session, &dir);

        let report = actor
            .project_once(Duration::from_secs(2))
            .await
            .expect("project once");

        assert_eq!(report.state.nodes.len(), 1);
        assert!(report.gateway_snapshot.is_some());
        assert!(report.dns_snapshot.is_some());
        assert!(dir.path().join("projections.sqlite").exists());
        assert!(dir.path().join("gateway.snapshot").exists());
        assert!(dir.path().join("dns.snapshot").exists());
    }

    #[tokio::test]
    async fn dropped_hint_does_not_block_full_projection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (bus, session) = seed_projectable_facts();
        let actor = actor_for(Arc::new(BusFactSource::new(bus)), session, &dir);

        actor.fact_hint().await.expect("fact hint");
        let report = actor
            .project_once(Duration::from_secs(2))
            .await
            .expect("project once");
        let status = actor.status().await.expect("status");

        assert_eq!(status.hints_seen, 1);
        assert_eq!(report.state.nodes.len(), 1);
    }

    #[tokio::test]
    async fn failure_preserves_last_success_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (bus, session) = seed_projectable_facts();
        let source = Arc::new(FlakySource::new(BusFactSource::new(bus)));
        let actor = actor_for(source.clone(), session, &dir);
        actor
            .project_once(Duration::from_secs(2))
            .await
            .expect("project once");
        source.fail();

        let error = actor
            .project_once(Duration::from_secs(2))
            .await
            .unwrap_err();
        let status = actor.status().await.expect("status");

        assert!(matches!(error, crate::ProjectionError::FactSource(_)));
        assert!(status.last_success.is_some());
        assert!(status.last_failure.is_some());
    }

    #[tokio::test]
    async fn slow_source_reports_deadline_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_bus, session) = seed_projectable_facts();
        let actor = actor_for(
            Arc::new(SlowSource(Duration::from_millis(75))),
            session,
            &dir,
        );

        let started = std::time::Instant::now();
        let error = actor
            .project_once(Duration::from_millis(10))
            .await
            .unwrap_err();

        assert!(matches!(error, crate::ProjectionError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_millis(100));
        let status = actor.status().await.expect("status");
        assert!(status.last_failure.is_some());
    }

    #[tokio::test]
    async fn status_remains_responsive_while_projection_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_bus, session) = seed_projectable_facts();
        let actor = actor_for(
            Arc::new(SlowSource(Duration::from_millis(75))),
            session,
            &dir,
        );
        let pending_actor = actor.clone();
        let pending =
            tokio::spawn(
                async move { pending_actor.project_once(Duration::from_millis(100)).await },
            );

        let status = tokio::time::timeout(Duration::from_millis(20), actor.status())
            .await
            .expect("status did not block on projection")
            .expect("status");

        assert_eq!(status.hints_seen, 0);
        pending
            .await
            .expect("projection task join")
            .expect("projection");
    }

    #[tokio::test]
    async fn timed_out_projection_keeps_inflight_guard_until_worker_exits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_bus, session) = seed_projectable_facts();
        let actor = actor_for(
            Arc::new(SlowSource(Duration::from_millis(250))),
            session,
            &dir,
        );

        let first = actor
            .project_once(Duration::from_millis(10))
            .await
            .unwrap_err();
        let second = actor
            .project_once(Duration::from_millis(10))
            .await
            .unwrap_err();

        assert!(matches!(first, crate::ProjectionError::Timeout { .. }));
        assert!(matches!(
            second,
            crate::ProjectionError::Timeout {
                operation: "project_once_inflight"
            }
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!dir.path().join("projections.sqlite").exists());
        assert!(!dir.path().join("gateway.snapshot").exists());
        assert!(!dir.path().join("dns.snapshot").exists());
        actor
            .project_once(Duration::from_millis(500))
            .await
            .expect("projection after old worker exits");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapshot_failure_does_not_promote_staged_sqlite() {
        use std::fs;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let (bus, session) = seed_projectable_facts();
        let actor = actor_for(
            Arc::new(BusFactSource::new(bus.clone())),
            session.clone(),
            &dir,
        );
        let first = actor
            .project_once(Duration::from_secs(2))
            .await
            .expect("initial projection");
        let sqlite = SqliteProjectionStore::new(dir.path().join("projections.sqlite"));

        write_payload(
            &bus,
            &session,
            "/facts/node/node-2/joined/1",
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-2"),
                epoch: 1,
                overlay_ip: "fd00::2".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        );
        let dns_path = dir.path().join("dns.snapshot");
        let symlink_target = dir.path().join("dns-target.snapshot");
        fs::write(&symlink_target, b"old").expect("write symlink target");
        fs::remove_file(&dns_path).expect("remove dns snapshot");
        symlink(&symlink_target, &dns_path).expect("replace dns snapshot with symlink");

        let error = actor
            .project_once(Duration::from_secs(2))
            .await
            .unwrap_err();
        let loaded = sqlite.load().expect("load sqlite after failed projection");

        assert!(matches!(error, crate::ProjectionError::SnapshotPath { .. }));
        assert_eq!(loaded, first.state);
    }

    struct FlakySource {
        fail: AtomicBool,
        inner: BusFactSource,
    }

    impl FlakySource {
        fn new(inner: BusFactSource) -> Self {
            Self {
                fail: AtomicBool::new(false),
                inner,
            }
        }

        fn fail(&self) {
            self.fail.store(true, Ordering::SeqCst);
        }
    }

    impl FactSource for FlakySource {
        fn list_candidates(
            &self,
            island: &IslandId,
            pattern: &FactKeyPattern,
            session: &BusSession,
        ) -> FactSourceResult<Vec<FactCandidate>> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(FactSourceError::Unavailable {
                    name: "flaky".to_string(),
                });
            }
            self.inner.list_candidates(island, pattern, session)
        }

        fn read_payloads(
            &self,
            island: &IslandId,
            candidates: &[FactCandidate],
            session: &BusSession,
        ) -> FactSourceResult<std::collections::BTreeMap<FactContentHash, FactPayload>> {
            self.inner.read_payloads(island, candidates, session)
        }
    }

    struct SlowSource(Duration);

    impl FactSource for SlowSource {
        fn list_candidates(
            &self,
            island: &IslandId,
            _pattern: &FactKeyPattern,
            _session: &BusSession,
        ) -> FactSourceResult<Vec<FactCandidate>> {
            thread::sleep(self.0);
            Ok(vec![FactCandidate::new(
                island.clone(),
                key("/facts/node/node-1/joined/1"),
                PrincipalId::new("projection"),
                FactContentHash::new("missing"),
                FactKind::NodeJoined,
                0,
                CandidateStatus::Verified,
            )])
        }

        fn read_payloads(
            &self,
            _island: &IslandId,
            _candidates: &[FactCandidate],
            _session: &BusSession,
        ) -> FactSourceResult<std::collections::BTreeMap<FactContentHash, FactPayload>> {
            Ok(std::collections::BTreeMap::new())
        }
    }
}
