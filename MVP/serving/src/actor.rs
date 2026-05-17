use std::time::{Duration, SystemTime};

use kameo::Actor;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::mailbox;
use kameo::message::{Context, Message};
use mvp_bus::IslandId;
use mvp_projection::{DnsRecordProjection, GatewayRouteProjection};

use crate::{
    ServingError, ServingFailure, ServingFreshness, ServingResult, ServingSnapshotBatch,
    ServingSnapshotPaths, ServingStatus,
};

const SERVING_ACTOR_MAILBOX_CAPACITY: usize = 16;
const SERVING_ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Actor)]
struct ServingActor {
    expected_island: IslandId,
    paths: ServingSnapshotPaths,
    stale_after: Duration,
    batch: ServingSnapshotBatch,
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
        batch: ServingSnapshotBatch,
        now: SystemTime,
    ) -> Self {
        Self {
            expected_island,
            paths,
            stale_after,
            batch,
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
        ServingStatus {
            loaded_revisions: self.batch.revisions(),
            loaded_at: self.loaded_at,
            snapshot_age,
            freshness,
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
}

impl ServingActorHandle {
    pub fn spawn(
        expected_island: IslandId,
        paths: ServingSnapshotPaths,
        stale_after: Duration,
    ) -> ServingResult<Self> {
        let now = SystemTime::now();
        let batch = ServingSnapshotBatch::load(&paths, &expected_island)?;
        Ok(Self {
            actor: ServingActor::spawn_with_mailbox(
                ServingActor::new(expected_island, paths, stale_after, batch, now),
                mailbox::bounded(SERVING_ACTOR_MAILBOX_CAPACITY),
            ),
        })
    }

    pub async fn gateway_route_for_host(
        &self,
        host: impl Into<String>,
    ) -> ServingResult<Option<GatewayRouteProjection>> {
        self.actor
            .ask(GatewayRouteForHost { host: host.into() })
            .mailbox_timeout(SERVING_ACTOR_TIMEOUT)
            .reply_timeout(SERVING_ACTOR_TIMEOUT)
            .await
            .map_err(|error| map_send_error("gateway_route_for_host", error))
    }

    pub async fn dns_records(
        &self,
        name: impl Into<String>,
        record_type: impl Into<String>,
    ) -> ServingResult<Vec<DnsRecordProjection>> {
        self.actor
            .ask(DnsRecords {
                name: name.into(),
                record_type: record_type.into(),
            })
            .mailbox_timeout(SERVING_ACTOR_TIMEOUT)
            .reply_timeout(SERVING_ACTOR_TIMEOUT)
            .await
            .map_err(|error| map_send_error("dns_records", error))
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

struct GatewayRouteForHost {
    host: String,
}

impl Message<GatewayRouteForHost> for ServingActor {
    type Reply = Option<GatewayRouteProjection>;

    async fn handle(
        &mut self,
        message: GatewayRouteForHost,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.batch.route_for_host(&message.host)
    }
}

struct DnsRecords {
    name: String,
    record_type: String,
}

impl Message<DnsRecords> for ServingActor {
    type Reply = Vec<DnsRecordProjection>;

    async fn handle(
        &mut self,
        message: DnsRecords,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.batch.dns_records(&message.name, &message.record_type)
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
                self.batch = batch;
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

fn map_send_error<T>(operation: &'static str, error: SendError<T>) -> ServingError {
    ServingError::ActorUnavailable {
        operation,
        reason: error.to_string(),
    }
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
