//! One stream of wakes over a set of Corrosion subscriptions.
//!
//! A subscriber re-reads its tables on a wake and ignores what the change carried. The
//! stream wakes on a change (after an optional quiet window), on a fixed refresh tick,
//! and after it reopens its subscriptions. A quiet table sends nothing, so a stalled
//! subscription looks like an idle one; the tick is what notices a snapshot still open
//! and degrades it to a resubscribe.

use std::{future::Future, time::Duration};

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{Error, Subscription};

/// Quiet window after a change, and the longest a steady stream of changes can defer
/// the wake. The quiet window resets on each change; the limit does not.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Debounce {
    pub(crate) quiet: Duration,
    pub(crate) limit: Duration,
}

/// What a watch does with a subscription that failed to open, ended or stalled.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Retry {
    /// Reopen after this wait, and wake the reader once it has reopened.
    InPlace(Duration),
    /// Hand the error to the reader, which ends its own stream.
    Surface,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WatchPolicy {
    pub(crate) debounce: Option<Debounce>,
    /// How often the reader re-reads whatever the tables do, and how soon a stalled
    /// snapshot is noticed.
    pub(crate) refresh: Duration,
    pub(crate) retry: Retry,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Wake {
    /// A table changed, or the subscriptions were just reopened and may have missed one.
    Changed,
    /// The refresh interval passed with every snapshot complete.
    Tick,
    Shutdown,
}

/// The subscriptions a reader holds, reopened through `open` when they fail.
///
/// `next` is cancel safe: a call dropped between wakes loses no change.
pub(crate) struct ChangeWatch<Open> {
    name: &'static str,
    open: Open,
    policy: WatchPolicy,
    shutdown: CancellationToken,
    held: Vec<Subscription>,
    refresh: tokio::time::Interval,
}

impl<Open, Opening> ChangeWatch<Open>
where
    Open: FnMut() -> Opening,
    Opening: Future<Output = Result<Vec<Subscription>, Error>>,
{
    /// A watch that opens its subscriptions on the first `next`, or on `connect`. `name`
    /// says whose subscriptions these are in the log.
    pub(crate) fn new(
        name: &'static str,
        open: Open,
        policy: WatchPolicy,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            name,
            open,
            policy,
            shutdown,
            held: Vec::new(),
            refresh: interval_from_now(policy.refresh),
        }
    }

    /// Open the subscriptions now, once, and report the failure to the caller whatever the
    /// retry policy is. A reader that must know the subscriptions exist before it reads
    /// uses this; `next` then does not report the opening as a wake.
    ///
    /// # Errors
    ///
    /// Returns the opener's error.
    pub(crate) async fn connect(&mut self) -> Result<(), Error> {
        let held = (self.open)().await?;
        self.opened(held);
        Ok(())
    }

    fn opened(&mut self, held: Vec<Subscription>) {
        self.held = held;
        self.refresh = interval_from_now(self.policy.refresh);
    }

    /// The next reason to re-read.
    ///
    /// # Errors
    ///
    /// Under [`Retry::Surface`], the opener's error, a subscription's error, or
    /// [`Error::SnapshotStalled`]. Under [`Retry::InPlace`] none: the watch reopens.
    pub(crate) async fn next(&mut self) -> Result<Wake, Error> {
        loop {
            if self.held.is_empty() {
                let opening = (self.open)();
                let opened = tokio::select! {
                    biased;
                    () = self.shutdown.cancelled() => return Ok(Wake::Shutdown),
                    opened = opening => opened,
                };
                match opened {
                    Ok(held) => {
                        self.opened(held);
                        return Ok(Wake::Changed);
                    }
                    Err(error) => {
                        let Retry::InPlace(wait) = self.policy.retry else {
                            return Err(error);
                        };
                        tracing::warn!(
                            watch = self.name,
                            error = %ployz_core::error_chain::inline(&error),
                            "subscription failed, retrying"
                        );
                        if !pause(&self.shutdown, wait).await {
                            return Ok(Wake::Shutdown);
                        }
                        continue;
                    }
                }
            }
            match self.wake().await {
                Ok(wake) => return Ok(wake),
                Err(error) => {
                    let Retry::InPlace(wait) = self.policy.retry else {
                        return Err(error);
                    };
                    self.held.clear();
                    tracing::warn!(
                        watch = self.name,
                        error = %ployz_core::error_chain::inline(&error),
                        "subscription ended, reopening"
                    );
                    // A stalled stream is no failure of the opener, so it reopens at once.
                    if !matches!(error, Error::SnapshotStalled)
                        && !pause(&self.shutdown, wait).await
                    {
                        return Ok(Wake::Shutdown);
                    }
                }
            }
        }
    }

    fn stalled(&self) -> bool {
        self.held.iter().any(Subscription::stalled)
    }

    async fn wake(&mut self) -> Result<Wake, Error> {
        tokio::select! {
            biased;
            () = self.shutdown.cancelled() => return Ok(Wake::Shutdown),
            _ = self.refresh.tick() => {
                return if self.stalled() { Err(Error::SnapshotStalled) } else { Ok(Wake::Tick) };
            }
            changed = any_changed(&mut self.held) => changed?,
        }
        let Some(Debounce { quiet, limit }) = self.policy.debounce else {
            return Ok(Wake::Changed);
        };
        let quiet_for = tokio::time::sleep(quiet);
        tokio::pin!(quiet_for);
        let limit = tokio::time::sleep(limit);
        tokio::pin!(limit);
        loop {
            tokio::select! {
                biased;
                () = self.shutdown.cancelled() => return Ok(Wake::Shutdown),
                () = &mut limit => return Ok(Wake::Changed),
                changed = any_changed(&mut self.held) => changed?,
                () = &mut quiet_for => {
                    if !self.stalled() {
                        return Ok(Wake::Changed);
                    }
                    // A snapshot cut short by the quiet window is not a table to read yet.
                    tokio::select! {
                        () = self.shutdown.cancelled() => return Ok(Wake::Shutdown),
                        changed = any_changed(&mut self.held) => changed?,
                        _ = self.refresh.tick() => return Err(Error::SnapshotStalled),
                    }
                }
            }
            quiet_for.as_mut().reset(Instant::now() + quiet);
        }
    }
}

/// Waits `wait`, or reports `false` if shutdown comes first.
async fn pause(shutdown: &CancellationToken, wait: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(wait) => true,
        () = shutdown.cancelled() => false,
    }
}

/// Resolves on the first change of any subscription. The order rotates so a subscription
/// that is always ready cannot starve the others.
async fn any_changed(held: &mut [Subscription]) -> Result<(), Error> {
    held.rotate_left(1);
    let changes = held
        .iter_mut()
        .map(|subscription| Box::pin(subscription.changed()));
    futures_util::future::select_all(changes).await.0
}

fn interval_from_now(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval_at(Instant::now() + period, period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io,
        sync::{Arc, Mutex},
    };

    use bytes::Bytes;
    use futures_util::{StreamExt, stream};
    use tokio_stream::wrappers::UnboundedReceiverStream;

    use super::*;

    const REFRESH: Duration = Duration::from_secs(30);
    const RETRY: Duration = Duration::from_secs(1);
    const CHANGE: &[u8] = b"{\"change\":null}\n";

    fn quiet() -> Subscription {
        Subscription::streaming(stream::pending::<Result<Bytes, io::Error>>())
    }

    /// A snapshot that opened and never ended.
    fn stalled() -> Subscription {
        Subscription::streaming(
            stream::iter([Ok::<_, io::Error>(Bytes::from_static(
                b"{\"columns\":[\"id\",\"info\"]}\n",
            ))])
            .chain(stream::pending()),
        )
    }

    fn closed() -> Subscription {
        Subscription::streaming(stream::empty::<Result<Bytes, io::Error>>())
    }

    /// A subscription that wakes on every `CHANGE` sent to the returned sender.
    fn feed() -> (
        tokio::sync::mpsc::UnboundedSender<Result<Bytes, io::Error>>,
        Subscription,
    ) {
        let (sender, received) = tokio::sync::mpsc::unbounded_channel();
        (
            sender,
            Subscription::streaming(UnboundedReceiverStream::new(received)),
        )
    }

    fn policy(retry: Retry, debounce: Option<Debounce>) -> WatchPolicy {
        WatchPolicy {
            debounce,
            refresh: REFRESH,
            retry,
        }
    }

    const DEBOUNCE: Debounce = Debounce {
        quiet: Duration::from_millis(200),
        limit: Duration::from_secs(2),
    };

    type Opened = Result<Vec<Subscription>, Error>;

    /// An opener that answers each call from `script` in order and counts the calls.
    #[derive(Clone, Default)]
    struct Script {
        script: Arc<Mutex<VecDeque<Opened>>>,
        opened: Arc<Mutex<usize>>,
    }

    impl Script {
        fn of(script: impl IntoIterator<Item = Result<Vec<Subscription>, Error>>) -> Self {
            Self {
                script: Arc::new(Mutex::new(script.into_iter().collect())),
                opened: Arc::default(),
            }
        }

        fn opener(
            &self,
        ) -> impl FnMut() -> std::future::Ready<Result<Vec<Subscription>, Error>> + use<> {
            let this = self.clone();
            move || {
                *this.opened.lock().unwrap() += 1;
                std::future::ready(
                    this.script
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("the watch opened more often than the test scripted"),
                )
            }
        }

        fn opens(&self) -> usize {
            *self.opened.lock().unwrap()
        }
    }

    fn watch(
        script: &Script,
        policy: WatchPolicy,
        shutdown: CancellationToken,
    ) -> ChangeWatch<impl FnMut() -> std::future::Ready<Result<Vec<Subscription>, Error>> + use<>>
    {
        ChangeWatch::new("test", script.opener(), policy, shutdown)
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_wake_is_the_opening_and_a_quiet_table_ticks_on_the_interval() {
        let script = Script::of([Ok(vec![quiet()])]);
        let mut changes = watch(
            &script,
            policy(Retry::InPlace(RETRY), None),
            CancellationToken::new(),
        );
        let started = Instant::now();

        assert_eq!(changes.next().await.unwrap(), Wake::Changed);
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(changes.next().await.unwrap(), Wake::Tick);
        assert_eq!(started.elapsed(), REFRESH);
        assert_eq!(changes.next().await.unwrap(), Wake::Tick);
        assert_eq!(started.elapsed(), REFRESH * 2);
        assert_eq!(script.opens(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_connected_watch_does_not_report_its_opening_as_a_wake() {
        let script = Script::of([Ok(vec![quiet()])]);
        let mut changes = watch(
            &script,
            policy(Retry::Surface, None),
            CancellationToken::new(),
        );
        let started = Instant::now();

        changes.connect().await.unwrap();

        assert_eq!(changes.next().await.unwrap(), Wake::Tick);
        assert_eq!(started.elapsed(), REFRESH);
        assert_eq!(script.opens(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn connect_reports_a_failed_opening_whatever_the_retry_policy() {
        let script = Script::of([Err(Error::Api("no subscriptions".into()))]);
        let mut changes = watch(
            &script,
            policy(Retry::InPlace(RETRY), None),
            CancellationToken::new(),
        );

        let error = changes.connect().await.unwrap_err();

        assert_eq!(error.to_string(), "Corrosion API error: no subscriptions");
    }

    #[tokio::test(start_paused = true)]
    async fn a_snapshot_still_open_at_the_tick_is_surfaced_as_a_stall() {
        let script = Script::of([Ok(vec![quiet(), stalled()])]);
        let mut changes = watch(
            &script,
            policy(Retry::Surface, None),
            CancellationToken::new(),
        );
        let started = Instant::now();

        assert_eq!(changes.next().await.unwrap(), Wake::Changed);
        let error = changes.next().await.unwrap_err();

        assert!(matches!(error, Error::SnapshotStalled), "{error}");
        assert_eq!(
            error.to_string(),
            "invalid Corrosion protocol response: subscription snapshot stalled"
        );
        assert_eq!(started.elapsed(), REFRESH);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_snapshot_is_reopened_at_once_and_wakes_the_reader() {
        let script = Script::of([Ok(vec![stalled()]), Ok(vec![quiet()])]);
        let mut changes = watch(
            &script,
            policy(Retry::InPlace(RETRY), None),
            CancellationToken::new(),
        );
        let started = Instant::now();

        assert_eq!(changes.next().await.unwrap(), Wake::Changed);
        assert_eq!(changes.next().await.unwrap(), Wake::Changed);

        assert_eq!(started.elapsed(), REFRESH, "a stall costs no retry wait");
        assert_eq!(script.opens(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn changes_on_another_table_do_not_put_off_the_stall_check() {
        let (sender, chatty) = feed();
        let script = Script::of([Ok(vec![chatty, stalled()])]);
        let mut changes = watch(
            &script,
            policy(Retry::Surface, None),
            CancellationToken::new(),
        );
        let writer = tokio::spawn(async move {
            loop {
                tokio::time::sleep(REFRESH / 3).await;
                if sender.send(Ok(Bytes::from_static(CHANGE))).is_err() {
                    return;
                }
            }
        });
        let started = Instant::now();

        let error = loop {
            if let Err(error) = changes.next().await {
                break error;
            }
        };

        assert!(matches!(error, Error::SnapshotStalled), "{error}");
        assert_eq!(started.elapsed(), REFRESH);
        writer.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn an_ended_subscription_is_reopened_after_the_retry_wait() {
        let script = Script::of([Ok(vec![closed()]), Ok(vec![quiet()])]);
        let mut changes = watch(
            &script,
            policy(Retry::InPlace(RETRY), None),
            CancellationToken::new(),
        );
        let started = Instant::now();

        assert_eq!(changes.next().await.unwrap(), Wake::Changed);
        assert_eq!(changes.next().await.unwrap(), Wake::Changed);

        assert_eq!(started.elapsed(), RETRY);
        assert_eq!(script.opens(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_opening_is_retried_after_the_retry_wait_until_it_opens() {
        let script = Script::of([
            Err(Error::Api("down".into())),
            Err(Error::Api("still down".into())),
            Ok(vec![quiet()]),
        ]);
        let mut changes = watch(
            &script,
            policy(Retry::InPlace(RETRY), None),
            CancellationToken::new(),
        );
        let started = Instant::now();

        assert_eq!(changes.next().await.unwrap(), Wake::Changed);

        assert_eq!(started.elapsed(), RETRY * 2);
        assert_eq!(script.opens(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn surface_hands_back_a_failed_opening_and_an_ended_subscription() {
        let script = Script::of([Err(Error::Api("down".into())), Ok(vec![closed()])]);
        let mut changes = watch(
            &script,
            policy(Retry::Surface, None),
            CancellationToken::new(),
        );

        let error = changes.next().await.unwrap_err();
        assert_eq!(error.to_string(), "Corrosion API error: down");

        assert_eq!(changes.next().await.unwrap(), Wake::Changed);
        let error = changes.next().await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid Corrosion protocol response: subscription stream closed"
        );
        assert_eq!(script.opens(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_change_wakes_the_reader_once_the_quiet_window_passes() {
        let (sender, subscription) = feed();
        let script = Script::of([Ok(vec![subscription])]);
        let mut changes = watch(
            &script,
            policy(Retry::Surface, Some(DEBOUNCE)),
            CancellationToken::new(),
        );
        assert_eq!(changes.next().await.unwrap(), Wake::Changed);
        let started = Instant::now();
        sender.send(Ok(Bytes::from_static(CHANGE))).unwrap();

        assert_eq!(changes.next().await.unwrap(), Wake::Changed);

        assert_eq!(started.elapsed(), DEBOUNCE.quiet);
    }

    #[tokio::test(start_paused = true)]
    async fn a_steady_stream_of_changes_cannot_defer_the_wake_past_the_limit() {
        let (sender, subscription) = feed();
        let script = Script::of([Ok(vec![subscription])]);
        let mut changes = watch(
            &script,
            policy(Retry::Surface, Some(DEBOUNCE)),
            CancellationToken::new(),
        );
        assert_eq!(changes.next().await.unwrap(), Wake::Changed);
        // Faster than the quiet window, for longer than the limit.
        let writer = tokio::spawn(async move {
            for _ in 0..60 {
                sender.send(Ok(Bytes::from_static(CHANGE))).unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        let started = Instant::now();

        let wake = tokio::time::timeout(Duration::from_millis(2900), changes.next())
            .await
            .expect("the limit wakes the reader while writes continue")
            .unwrap();

        assert_eq!(wake, Wake::Changed);
        assert_eq!(started.elapsed(), DEBOUNCE.limit);
        assert!(!writer.is_finished(), "the wake came while writes flowed");
        writer.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_snapshot_cut_short_by_the_quiet_window_is_not_a_wake() {
        let (sender, subscription) = feed();
        let script = Script::of([Ok(vec![subscription])]);
        let mut changes = watch(
            &script,
            policy(Retry::Surface, Some(DEBOUNCE)),
            CancellationToken::new(),
        );
        assert_eq!(changes.next().await.unwrap(), Wake::Changed);
        let started = Instant::now();
        sender.send(Ok(Bytes::from_static(CHANGE))).unwrap();
        // A snapshot begins after the change and does not end.
        sender
            .send(Ok(Bytes::from_static(
                b"{\"columns\":[\"id\"]}\n{\"row\":[1,[1]]}\n",
            )))
            .unwrap();

        let error = changes.next().await.unwrap_err();

        assert!(matches!(error, Error::SnapshotStalled), "{error}");
        assert_eq!(started.elapsed(), REFRESH);
    }

    /// Cancels `shutdown` while `changes` is in its next wait and reports what woke it.
    async fn shut_down_during<Open, Opening>(
        mut changes: ChangeWatch<Open>,
        shutdown: CancellationToken,
    ) -> Wake
    where
        Open: FnMut() -> Opening + Send + 'static,
        Opening: Future<Output = Result<Vec<Subscription>, Error>> + Send + 'static,
    {
        let waiting = tokio::spawn(async move { changes.next().await.unwrap() });
        tokio::time::sleep(Duration::from_millis(100)).await;
        shutdown.cancel();
        waiting.await.unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_ends_the_opening_the_wait_the_debounce_and_the_retry_wait() {
        // While the subscriptions open.
        let shutdown = CancellationToken::new();
        let opening = ChangeWatch::new(
            "test",
            std::future::pending::<Result<Vec<Subscription>, Error>>,
            policy(Retry::InPlace(RETRY), None),
            shutdown.clone(),
        );
        assert_eq!(shut_down_during(opening, shutdown).await, Wake::Shutdown);

        // While waiting for a change.
        let shutdown = CancellationToken::new();
        let script = Script::of([Ok(vec![quiet()])]);
        let mut waiting = watch(&script, policy(Retry::Surface, None), shutdown.clone());
        assert_eq!(waiting.next().await.unwrap(), Wake::Changed);
        assert_eq!(shut_down_during(waiting, shutdown).await, Wake::Shutdown);

        // Between a change and the end of its quiet window.
        let shutdown = CancellationToken::new();
        let (sender, subscription) = feed();
        let script = Script::of([Ok(vec![subscription])]);
        let mut debouncing = watch(
            &script,
            policy(
                Retry::Surface,
                Some(Debounce {
                    quiet: Duration::from_secs(5),
                    limit: Duration::from_secs(9),
                }),
            ),
            shutdown.clone(),
        );
        assert_eq!(debouncing.next().await.unwrap(), Wake::Changed);
        sender.send(Ok(Bytes::from_static(CHANGE))).unwrap();
        assert_eq!(shut_down_during(debouncing, shutdown).await, Wake::Shutdown);

        // While waiting to retry a failed opening.
        let shutdown = CancellationToken::new();
        let script = Script::of([Err(Error::Api("down".into()))]);
        let retrying = watch(
            &script,
            policy(Retry::InPlace(RETRY * 10), None),
            shutdown.clone(),
        );
        assert_eq!(shut_down_during(retrying, shutdown).await, Wake::Shutdown);
        assert_eq!(script.opens(), 1);
    }
}
