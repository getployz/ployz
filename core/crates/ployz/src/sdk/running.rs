//! Cancellable SDK calls whose progress is retained until read, within a byte budget.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ployz_core::{RpcError, RpcErrorCode};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::invalid_argument;
use super::prepare::Progress;

/// Most build output retained ahead of a slow consumer before it is dropped.
const OUTPUT_BUDGET: usize = 64 * 1024 * 1024;

const DROPPED_MARKER: &str = "… output dropped: the consumer fell behind\n";

/// Drops output beyond the budget, marking the first drop on the step it hit,
/// until the consumer is back under half the budget. Structured progress
/// always passes.
#[derive(Default)]
struct OutputBudget {
    dropping: bool,
}

impl OutputBudget {
    /// The frame to send with its accounted size, or none when dropped.
    fn frame(&mut self, mut progress: Progress, buffered: &AtomicUsize) -> Option<(usize, Value)> {
        use ployz_build::Progress as Build;
        let held = buffered.load(Ordering::Relaxed);
        if held < OUTPUT_BUDGET / 2 {
            self.dropping = false;
        }
        let size = match &mut progress {
            Progress::Build(Build::StepOutput { text, .. }) => {
                match self.admit(held, text.len())? {
                    Admitted::Marker => *text = DROPPED_MARKER.into(),
                    Admitted::Output => {}
                }
                text.len()
            }
            Progress::Build(Build::Output(bytes)) => {
                match self.admit(held, bytes.len())? {
                    Admitted::Marker => *bytes = DROPPED_MARKER.as_bytes().to_vec(),
                    Admitted::Output => {}
                }
                bytes.len()
            }
            Progress::Build(
                Build::Stage(_) | Build::Step(_) | Build::Timing { .. } | Build::Target { .. },
            )
            | Progress::Platforms(_)
            | Progress::Selected(_)
            | Progress::Transfer
            | Progress::Delivered { .. } => 0,
        };
        buffered.fetch_add(size, Ordering::Relaxed);
        let value = serde_json::to_value(progress).expect("preparation progress serializes");
        Some((size, value))
    }

    /// Whether output of `len` bytes may be sent, or none while dropping.
    /// Dropping continues until the consumer is under half the budget so the
    /// stream cannot alternate between passing and silently dropping.
    fn admit(&mut self, held: usize, len: usize) -> Option<Admitted> {
        if self.dropping {
            return None;
        }
        if held + len > OUTPUT_BUDGET {
            self.dropping = true;
            return Some(Admitted::Marker);
        }
        Some(Admitted::Output)
    }
}

enum Admitted {
    Output,
    Marker,
}

/// Budgeted progress producer for a [`Running`] call.
pub(super) struct Reporter {
    events: tokio::sync::mpsc::UnboundedSender<(usize, Value)>,
    buffered: Arc<AtomicUsize>,
    budget: std::sync::Mutex<OutputBudget>,
}

impl Reporter {
    pub(super) fn report(&self, progress: Progress) {
        let frame = self
            .budget
            .lock()
            .expect("budgeting never panics while holding the marker flag")
            .frame(progress, &self.buffered);
        if let Some(frame) = frame {
            let _ = self.events.send(frame);
        }
    }
}

/// A cancellable SDK call whose progress is retained until read, within a byte budget.
pub struct Running<T> {
    cancel: CancellationToken,
    events: Mutex<tokio::sync::mpsc::UnboundedReceiver<(usize, Value)>>,
    buffered: Arc<AtomicUsize>,
    join: Mutex<Option<tokio::task::JoinHandle<Result<T, RpcError>>>>,
}
impl<T: Send + 'static> Running<T> {
    pub(super) fn spawn<F>(cancel: CancellationToken, work: impl FnOnce(Reporter) -> F) -> Self
    where
        F: std::future::Future<Output = Result<T, RpcError>> + Send + 'static,
    {
        // Lossless while the consumer keeps up: structured state is never
        // dropped, and output is only replaced by a marker beyond the budget.
        let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
        let buffered = Arc::new(AtomicUsize::new(0));
        let join = tokio::spawn(work(Reporter {
            events,
            buffered: Arc::clone(&buffered),
            budget: std::sync::Mutex::new(OutputBudget::default()),
        }));
        Self {
            cancel,
            events: Mutex::new(receiver),
            buffered,
            join: Mutex::new(Some(join)),
        }
    }
}
impl<T> Running<T> {
    /// Request cancellation; finished reports whether remote termination was confirmed.
    pub fn abort(&self) {
        self.cancel.cancel();
    }
    /// Read one progress frame; frames are retained until read, within the budget.
    pub async fn next(&self) -> Option<Value> {
        let (size, value) = self.events.lock().await.recv().await?;
        self.buffered.fetch_sub(size, Ordering::Relaxed);
        Some(value)
    }
    /// Await the result without draining or blocking on progress consumption.
    ///
    /// # Errors
    /// Returns the call's typed failure or unknown outcome, or rejects a second await.
    pub async fn finished(&self) -> Result<T, RpcError> {
        let join = self
            .join
            .lock()
            .await
            .take()
            .ok_or_else(|| invalid_argument("result already awaited".into()))?;
        join.await.map_err(|_| RpcError {
            code: RpcErrorCode::Internal,
            message: "SDK call task failed".into(),
            details: Value::Null,
        })?
    }
}
impl<T> Drop for Running<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn slow_preparation_consumer_loses_nothing_and_cannot_block_completion() {
        let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
        let join = tokio::spawn(async move {
            for n in 0..1000 {
                events.send((1, serde_json::json!({"n":n}))).unwrap();
            }
            Err::<(), _>(invalid_argument("fixture failure".into()))
        });
        let running = Running {
            cancel: CancellationToken::new(),
            events: Mutex::new(receiver),
            buffered: Arc::new(AtomicUsize::new(1000)),
            join: Mutex::new(Some(join)),
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), running.finished())
            .await
            .unwrap();
        assert_eq!(result.unwrap_err().message, "fixture failure");
        let mut count = 0;
        while running.next().await.is_some() {
            count += 1;
        }
        assert_eq!(count, 1000);
        assert_eq!(running.buffered.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn output_beyond_the_budget_becomes_one_marker_until_the_consumer_catches_up() {
        use ployz_build::Progress as Build;
        let buffered = AtomicUsize::new(OUTPUT_BUDGET);
        let mut budget = OutputBudget::default();
        let output = |text: &str| {
            Progress::Build(Build::StepOutput {
                step: "sha256:a".into(),
                stderr: false,
                text: text.into(),
            })
        };
        let (size, marker) = budget.frame(output("cargo output\n"), &buffered).unwrap();
        assert!(marker.to_string().contains("output dropped"), "{marker}");
        assert!(size > 0);
        assert!(budget.frame(output("more\n"), &buffered).is_none());
        // Structured state always passes.
        let (_, step) = budget
            .frame(
                Progress::Build(Build::Step(ployz_build::BuildStep::default())),
                &buffered,
            )
            .unwrap();
        assert!(step.get("Build").is_some());
        // Still dropping above half the budget, even though this frame would fit.
        buffered.store(OUTPUT_BUDGET / 2 + 1, Ordering::Relaxed);
        assert!(budget.frame(output("between\n"), &buffered).is_none());
        buffered.store(0, Ordering::Relaxed);
        let (size, value) = budget.frame(output("after\n"), &buffered).unwrap();
        assert_eq!(size, "after\n".len());
        assert!(value.to_string().contains("after"), "{value}");
    }
}
