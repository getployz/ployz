//! Secret-free timings for one Cloud management dial.

use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::Serialize;

tokio::task_local! {
    static CURRENT: Arc<Mutex<DialTimings>>;
}

/// The route selected when Iroh completes its QUIC connect. It can change later.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    Direct,
    Relayed,
    #[default]
    Unknown,
}

/// The last observation of a pending management dial.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DialOutcome {
    Answered,
    Cancelled,
    TimedOut,
    Refused,
    Unanswered,
}

/// Retain a full record for sampled, slow, or non-answering dials.
#[must_use]
pub fn should_emit(sampled: bool, total_ms: u64, outcome: DialOutcome) -> bool {
    sampled || total_ms >= 2_000 || outcome != DialOutcome::Answered
}

impl PathKind {
    pub(super) fn selected(connection: &iroh::endpoint::Connection) -> Self {
        let paths = connection.paths();
        let Some(path) = paths.iter().find(|path| path.is_selected()) else {
            return Self::Unknown;
        };
        Self::from_flags(path.is_ip(), path.is_relay())
    }

    fn from_flags(direct: bool, relayed: bool) -> Self {
        match (direct, relayed) {
            (true, false) => Self::Direct,
            (false, true) => Self::Relayed,
            _ => Self::Unknown,
        }
    }
}

/// Stages observed by the native SDK. A missing duration means the stage was not reached.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DialTimings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_bind_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iroh_connect_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http2_channel_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe_describe_contract_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_describe_contract_ms: Option<u64>,
    pub path_kind: PathKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    unfinished_stage: Option<Stage>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Stage {
    EndpointBind,
    IrohConnect,
    Proof,
    Http2Channel,
    ProbeDescribeContract,
    EntryDescribeContract,
}

pub(super) struct StageGuard {
    stage: Stage,
    started: Instant,
    finished: bool,
    timings: Option<Arc<Mutex<DialTimings>>>,
}

impl StageGuard {
    pub(super) fn begin(stage: Stage) -> Self {
        Self {
            stage,
            started: Instant::now(),
            finished: false,
            timings: CURRENT.try_with(Arc::clone).ok(),
        }
    }

    pub(super) fn finish(&mut self) {
        self.record(false);
        self.finished = true;
    }

    fn record(&self, unfinished: bool) {
        let ms = millis(self.started.elapsed());
        let Some(timings) = &self.timings else {
            return;
        };
        if let Ok(mut timings) = timings.lock() {
            match self.stage {
                Stage::EndpointBind => timings.endpoint_bind_ms = Some(ms),
                Stage::IrohConnect => timings.iroh_connect_ms = Some(ms),
                Stage::Proof => timings.proof_ms = Some(ms),
                Stage::Http2Channel => timings.http2_channel_ms = Some(ms),
                Stage::ProbeDescribeContract => timings.probe_describe_contract_ms = Some(ms),
                Stage::EntryDescribeContract => timings.entry_describe_contract_ms = Some(ms),
            }
            if unfinished {
                timings.unfinished_stage = Some(self.stage);
            }
        }
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.record(true);
        }
    }
}

/// Keep the collector scoped to the native pending dial, even across awaited stages.
pub async fn scope<T>(timings: Arc<Mutex<DialTimings>>, future: impl Future<Output = T>) -> T {
    CURRENT.scope(timings, future).await
}

pub(super) fn record(update: impl FnOnce(&mut DialTimings)) {
    let _ = CURRENT.try_with(|timings| {
        if let Ok(mut timings) = timings.lock() {
            update(&mut timings);
        }
    });
}

pub(super) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{DialOutcome, DialTimings, PathKind, Stage, StageGuard, scope, should_emit};
    use std::sync::{Arc, Mutex};

    #[test]
    fn selected_path_is_only_a_kind() {
        assert_eq!(PathKind::from_flags(true, false), PathKind::Direct);
        assert_eq!(PathKind::from_flags(false, true), PathKind::Relayed);
        assert_eq!(PathKind::from_flags(false, false), PathKind::Unknown);
        assert_eq!(PathKind::from_flags(true, true), PathKind::Unknown);
    }

    #[test]
    fn slow_and_non_answering_dials_are_always_kept() {
        assert!(should_emit(false, 2_001, DialOutcome::Answered));
        assert!(should_emit(false, 40, DialOutcome::TimedOut));
        assert!(should_emit(false, 40, DialOutcome::Cancelled));
        assert!(!should_emit(false, 40, DialOutcome::Answered));
    }

    #[tokio::test]
    async fn cancelled_stage_keeps_its_duration_and_name() {
        let timings = Arc::new(Mutex::new(DialTimings::default()));
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(scope(Arc::clone(&timings), async move {
            let _stage = StageGuard::begin(Stage::IrohConnect);
            let _ = entered.send(());
            std::future::pending::<()>().await;
        }));
        waiting.await.expect("stage entered");
        task.abort();
        let _ = task.await;
        let recorded = timings.lock().expect("timings lock");
        assert!(recorded.iroh_connect_ms.is_some());
        assert!(matches!(
            recorded.unfinished_stage,
            Some(Stage::IrohConnect)
        ));
    }
}
