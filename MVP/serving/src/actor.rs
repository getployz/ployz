use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, SystemTime};

use kameo::Actor;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::mailbox;
use kameo::message::{Context, Message};
use mvp_acme::AcmeKeyAuthorization;
use mvp_bus::IslandId;
use mvp_projection::{DnsRecordProjection, GatewayRouteProjection};

use crate::{
    ServingError, ServingFailure, ServingFreshness, ServingResult, ServingSnapshotBatch,
    ServingSnapshotPaths, ServingStatus,
};

const SERVING_ACTOR_MAILBOX_CAPACITY: usize = 16;
const SERVING_ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

type SharedSnapshots = Arc<RwLock<ServingSnapshotBatch>>;

#[derive(Actor)]
struct ServingActor {
    expected_island: IslandId,
    paths: ServingSnapshotPaths,
    stale_after: Duration,
    snapshots: SharedSnapshots,
    loaded_at: SystemTime,
    reload_attempts: u64,
    last_reload_attempt_at: Option<SystemTime>,
    last_reload_success_at: Option<SystemTime>,
    last_failure: Option<ServingFailure>,
}

impl ServingActor {
    fn new(
        expected_island: IslandId,
        paths: ServingSnapshotPaths,
        stale_after: Duration,
        snapshots: SharedSnapshots,
        now: SystemTime,
    ) -> Self {
        Self {
            expected_island,
            paths,
            stale_after,
            snapshots,
            loaded_at: now,
            reload_attempts: 0,
            last_reload_attempt_at: None,
            last_reload_success_at: Some(now),
            last_failure: None,
        }
    }

    fn status_at(&self, now: SystemTime) -> ServingStatus {
        let snapshot_age = now.duration_since(self.loaded_at).unwrap_or_default();
        let freshness = if self.last_failure.is_some() {
            ServingFreshness::ServingLastGoodAfterFailure
        } else if snapshot_age >= self.stale_after {
            ServingFreshness::ServingAgedSnapshot
        } else {
            ServingFreshness::Fresh
        };
        let snapshots = read_snapshots(&self.snapshots);
        ServingStatus {
            loaded_revisions: snapshots.revisions(),
            loaded_at: self.loaded_at,
            snapshot_age,
            freshness,
            acme_http01_challenge_count: snapshots.acme_http01_challenge_count(),
            reload_attempts: self.reload_attempts,
            last_reload_attempt_at: self.last_reload_attempt_at,
            last_reload_success_at: self.last_reload_success_at,
            last_failure: self.last_failure.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ServingActorHandle {
    actor: ActorRef<ServingActor>,
    snapshots: SharedSnapshots,
}

impl ServingActorHandle {
    pub fn spawn(
        expected_island: IslandId,
        paths: ServingSnapshotPaths,
        stale_after: Duration,
    ) -> ServingResult<Self> {
        let now = SystemTime::now();
        let batch = ServingSnapshotBatch::load(&paths, &expected_island)?;
        let snapshots = Arc::new(RwLock::new(batch));
        Ok(Self {
            snapshots: Arc::clone(&snapshots),
            actor: ServingActor::spawn_with_mailbox(
                ServingActor::new(expected_island, paths, stale_after, snapshots, now),
                mailbox::bounded(SERVING_ACTOR_MAILBOX_CAPACITY),
            ),
        })
    }

    pub async fn gateway_route_for_host(
        &self,
        host: impl Into<String>,
    ) -> ServingResult<Option<GatewayRouteProjection>> {
        Ok(read_snapshots(&self.snapshots).route_for_host(&host.into()))
    }

    pub async fn dns_records(
        &self,
        name: impl Into<String>,
        record_type: impl Into<String>,
    ) -> ServingResult<Vec<DnsRecordProjection>> {
        Ok(read_snapshots(&self.snapshots).dns_records(&name.into(), &record_type.into()))
    }

    pub async fn acme_http01_challenge(
        &self,
        host: impl Into<String>,
        token: impl Into<String>,
    ) -> ServingResult<Option<AcmeKeyAuthorization>> {
        read_snapshots(&self.snapshots).acme_http01_challenge(&host.into(), &token.into())
    }

    pub async fn reload(&self) -> ServingResult<ServingStatus> {
        self.actor
            .ask(ReloadSnapshots)
            .mailbox_timeout(SERVING_ACTOR_TIMEOUT)
            .reply_timeout(SERVING_ACTOR_TIMEOUT)
            .await
            .map_err(|error| map_serving_send_error("reload", error))
    }

    pub async fn status(&self) -> ServingResult<ServingStatus> {
        self.actor
            .ask(ReadStatus)
            .mailbox_timeout(SERVING_ACTOR_TIMEOUT)
            .reply_timeout(SERVING_ACTOR_TIMEOUT)
            .await
            .map_err(|error| map_serving_send_error("status", error))
    }
}

struct ReloadSnapshots;

impl Message<ReloadSnapshots> for ServingActor {
    type Reply = ServingResult<ServingStatus>;

    async fn handle(
        &mut self,
        _message: ReloadSnapshots,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let now = SystemTime::now();
        self.reload_attempts += 1;
        self.last_reload_attempt_at = Some(now);
        match ServingSnapshotBatch::load(&self.paths, &self.expected_island) {
            Ok(batch) => {
                *write_snapshots(&self.snapshots) = batch;
                self.loaded_at = now;
                self.last_reload_success_at = Some(now);
                self.last_failure = None;
                Ok(self.status_at(now))
            }
            Err(error) => {
                self.last_failure = Some(error.failure());
                Err(error)
            }
        }
    }
}

struct ReadStatus;

impl Message<ReadStatus> for ServingActor {
    type Reply = ServingResult<ServingStatus>;

    async fn handle(
        &mut self,
        _message: ReadStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.status_at(SystemTime::now()))
    }
}

fn read_snapshots(snapshots: &SharedSnapshots) -> RwLockReadGuard<'_, ServingSnapshotBatch> {
    snapshots.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_snapshots(snapshots: &SharedSnapshots) -> RwLockWriteGuard<'_, ServingSnapshotBatch> {
    snapshots.write().unwrap_or_else(PoisonError::into_inner)
}

fn map_serving_send_error<T>(
    operation: &'static str,
    error: SendError<T, ServingError>,
) -> ServingError {
    match error {
        SendError::HandlerError(error) => error,
        SendError::ActorNotRunning(_)
        | SendError::ActorStopped
        | SendError::MailboxFull(_)
        | SendError::Timeout(_) => ServingError::ActorUnavailable {
            operation,
            reason: error.to_string(),
        },
    }
}
