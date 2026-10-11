//! The handle the link layer holds on the engine. It owns no check state; every call becomes a
//! message to the engine task and, where an answer is needed, waits for it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rustls::crypto::CryptoProvider;
use tokio::sync::{Notify, mpsc, oneshot, watch};

use super::engine::{Deps, Engine, FeedCmd, Msg};
use super::feed::CheckFeed;
use super::incidents::IncidentWriter;
use super::pg::{Pg, PgError};
use super::results::{self, Results};
use crate::config::ControlConfig;
use crate::domain::Region;
use crate::link::http::PlaneApi;
use crate::link::{Pulled, Rejected, ResultBatch};
use crate::run::{Fatal, Shutdown};

const MSG_QUEUE: usize = 4096;
const WRITTEN_QUEUE: usize = 1024;
const FEED_POLL: Duration = Duration::from_secs(1);
const FEED_BACKOFF: Duration = Duration::from_millis(200);
const FEED_BACKOFF_MAX: Duration = Duration::from_secs(5);
const LOAD_RETRY_MAX: Duration = Duration::from_secs(10);

pub struct ControlPlane {
    msgs: mpsc::Sender<Msg>,
    wake: HashMap<Region, Arc<Notify>>,
    ready: watch::Receiver<bool>,
    stopped: watch::Receiver<bool>,
    regions: Vec<Region>,
}

impl ControlPlane {
    /// Connects to Postgres, starts the engine and loads the checks. Returns once the first
    /// load has succeeded or been handed to a retry loop.
    pub async fn start(
        config: ControlConfig,
        crypto: Arc<CryptoProvider>,
        shutdown: Shutdown,
    ) -> Result<ControlPlane, Fatal> {
        boot(config, crypto, shutdown, Engine::new).await
    }

    #[cfg(test)]
    pub(crate) async fn start_with(
        config: ControlConfig,
        crypto: Arc<CryptoProvider>,
        shutdown: Shutdown,
        tempo: super::engine::Tempo,
    ) -> Result<ControlPlane, Fatal> {
        boot(config, crypto, shutdown, move |regions, deps| {
            Engine::new(regions, deps).with_tempo(tempo)
        })
        .await
    }

    /// Jobs for `region`, waiting up to `wait` for some to appear.
    pub async fn pull(&self, region: &Region, want: u32, wait: Duration) -> Pulled {
        if !self.ready() {
            tokio::time::sleep(wait).await;
            return Pulled::empty();
        }
        let Some(wake) = self.wake.get(region) else {
            return Pulled::empty();
        };
        // Registered before the first take so a notify in between is not lost.
        let notified = wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let first = self.take(region, want).await;
        if !first.jobs.is_empty() {
            return first;
        }
        drop(tokio::time::timeout(wait, notified).await);
        self.take(region, want).await
    }

    async fn take(&self, region: &Region, want: u32) -> Pulled {
        let (reply, answer) = oneshot::channel();
        let msg = Msg::Take {
            region: region.clone(),
            want,
            reply,
        };
        if self.msgs.send(msg).await.is_err() {
            return Pulled::empty();
        }
        answer.await.unwrap_or_else(|_| Pulled::empty())
    }

    pub async fn report(&self, region: &Region, batch: ResultBatch) -> Result<(), Rejected> {
        if !self.ready() {
            return Err(Rejected::NotReady);
        }
        let (reply, answer) = oneshot::channel();
        let msg = Msg::Observed {
            region: region.clone(),
            batch: batch.id,
            observations: batch.observations,
            reply,
        };
        if self.msgs.send(msg).await.is_err() {
            return Err(Rejected::NotReady);
        }
        answer.await.map_err(|_| Rejected::NotReady)
    }

    #[must_use]
    pub fn ready(&self) -> bool {
        *self.ready.borrow()
    }

    #[must_use]
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// Resolves once the engine has drained and exited.
    pub async fn stopped(&self) {
        let mut stopped = self.stopped.clone();
        drop(stopped.wait_for(|done| *done).await);
    }
}

impl PlaneApi for ControlPlane {
    async fn pull(&self, region: &Region, want: u32, wait: Duration) -> Pulled {
        ControlPlane::pull(self, region, want, wait).await
    }

    async fn report(&self, region: &Region, batch: ResultBatch) -> Result<(), Rejected> {
        ControlPlane::report(self, region, batch).await
    }

    fn ready(&self) -> bool {
        ControlPlane::ready(self)
    }
}

async fn boot(
    config: ControlConfig,
    crypto: Arc<CryptoProvider>,
    shutdown: Shutdown,
    engine: impl FnOnce(Vec<Region>, Deps) -> Engine,
) -> Result<ControlPlane, Fatal> {
    let url = config.database_url.expose();
    let writer_pg = connect(url, &crypto, "the incident writer").await?;
    let results_pg = connect(url, &crypto, "the result writer").await?;
    let feed_pg = connect(url, &crypto, "the check feed").await?;
    tokio::spawn(super::region_names::write_until_done(
        feed_pg.clone(),
        config.regions.clone(),
    ));

    let (written_tx, written_rx) = mpsc::channel(WRITTEN_QUEUE);
    let writer = IncidentWriter::spawn(writer_pg, IncidentWriter::QUEUE, written_tx);
    let results = Results::spawn(results_pg, results::Config::PRODUCTION);
    let (msgs, msgs_rx) = mpsc::channel(MSG_QUEUE);
    let (feed_cmds, cmds_rx) = mpsc::channel(2);
    let regions = config.region_codes();
    let wake: HashMap<Region, Arc<Notify>> = regions
        .iter()
        .map(|r| (r.clone(), Arc::new(Notify::new())))
        .collect();
    let (ready_tx, ready) = watch::channel(false);
    let (stopped_tx, stopped) = watch::channel(false);

    let deps = Deps {
        writer,
        results,
        feed_cmds,
        wake: wake.clone(),
    };
    tokio::spawn(engine(regions.clone(), deps).run(msgs_rx, written_rx, stopped_tx));

    match CheckFeed::load(feed_pg.clone()).await {
        Ok((feed, snapshot)) => {
            drop(
                msgs.send(Msg::Feed(super::feed::FeedEvent::Loaded(snapshot)))
                    .await,
            );
            ready_tx.send_replace(true);
            tokio::spawn(follow(feed, msgs.clone(), cmds_rx));
        }
        Err(PgError::Fatal(e)) => return Err(Fatal(format!("Loading checks failed: {e}."))),
        Err(e) => {
            tracing::warn!(error = %e, "checks not loaded yet; retrying");
            tokio::spawn(load_until_ready(feed_pg, msgs.clone(), cmds_rx, ready_tx));
        }
    }

    let drain = msgs.clone();
    tokio::spawn(async move {
        shutdown.wait().await;
        drop(drain.send(Msg::Drain).await);
    });

    Ok(ControlPlane {
        msgs,
        wake,
        ready,
        stopped,
        regions,
    })
}

async fn connect(url: &str, crypto: &Arc<CryptoProvider>, role: &str) -> Result<Pg, Fatal> {
    Pg::connect(url, crypto.clone())
        .await
        .map_err(|e| Fatal(format!("DATABASE_URL: {role} cannot connect: {e}")))
}

async fn load_until_ready(
    pg: Pg,
    msgs: mpsc::Sender<Msg>,
    cmds: mpsc::Receiver<FeedCmd>,
    ready: watch::Sender<bool>,
) {
    let mut wait = Duration::from_secs(1);
    loop {
        tokio::time::sleep(wait).await;
        match CheckFeed::load(pg.clone()).await {
            Ok((feed, snapshot)) => {
                if msgs
                    .send(Msg::Feed(super::feed::FeedEvent::Loaded(snapshot)))
                    .await
                    .is_err()
                {
                    return;
                }
                ready.send_replace(true);
                follow(feed, msgs, cmds).await;
                return;
            }
            Err(PgError::Fatal(e)) => {
                tracing::error!(error = %e, "loading checks failed; this plane stays not ready");
                return;
            }
            Err(e) => tracing::warn!(error = %e, "checks not loaded yet; retrying"),
        }
        wait = (wait * 2).min(LOAD_RETRY_MAX);
    }
}

/// Polls the feed for changes and reloads on request, until the engine goes away.
async fn follow(mut feed: CheckFeed, msgs: mpsc::Sender<Msg>, mut cmds: mpsc::Receiver<FeedCmd>) {
    let mut backoff = FEED_BACKOFF;
    loop {
        let step = tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(FeedCmd::Reload) => feed.reload().await,
                None => return,
            },
            () = tokio::time::sleep(FEED_POLL) => feed.next().await,
        };
        match step {
            Ok(event) => {
                backoff = FEED_BACKOFF;
                if msgs.send(Msg::Feed(event)).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "check feed failed; retrying");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(FEED_BACKOFF_MAX);
            }
        }
    }
}
