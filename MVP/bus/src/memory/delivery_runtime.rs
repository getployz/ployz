use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{
    Receiver as DeliveryReceiver, SendTimeoutError, Sender as DeliverySender, TrySendError,
};

use super::{BusRuntimeConfig, BusRuntimeSnapshot, Handler, RequestContext, remaining_until};
use crate::message::{ReplyInbox, ReplyPermit};
use crate::{BusError, BusMessage, PrincipalId, ResponseEnvelope, Result};

#[derive(Clone)]
pub(super) struct ReplySpec {
    pub(super) inbox: ReplyInbox,
    pub(super) expires_at: Instant,
    pub(super) tx: mpsc::Sender<ResponseEnvelope>,
}

pub(super) struct Delivery {
    subscriber_id: u64,
    principal: PrincipalId,
    handler: Handler,
    message: BusMessage,
    reply: Option<ReplySpec>,
}

impl Delivery {
    pub(super) fn new(
        subscriber_id: u64,
        principal: PrincipalId,
        handler: Handler,
        message: BusMessage,
        reply: Option<ReplySpec>,
    ) -> Self {
        Self {
            subscriber_id,
            principal,
            handler,
            message,
            reply,
        }
    }

    fn invoke(self) -> Result<()> {
        let error_tx = self.reply.as_ref().map(|reply| reply.tx.clone());
        let handler = Arc::clone(&self.handler);
        let context = self.into_context();
        match handler(context) {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Some(tx) = error_tx {
                    let _ = tx.send(ResponseEnvelope::HandlerError(error.clone()));
                }
                Err(error)
            }
        }
    }

    fn into_context(self) -> RequestContext {
        let permit = self.reply.as_ref().map(|reply| {
            ReplyPermit::new(
                reply.inbox.clone(),
                self.message.id(),
                self.message.island().clone(),
                self.principal.clone(),
                reply.expires_at,
                reply.tx.clone(),
            )
        });
        let mut message = self.message;
        if let Some(reply) = &self.reply {
            message.set_reply_to(reply.inbox.clone());
        }
        let _subscriber_id = self.subscriber_id;
        RequestContext::new(message, permit)
    }
}

pub(super) struct DeliveryRuntime {
    config: BusRuntimeConfig,
    sender: DeliverySender<DeliveryJob>,
    inflight: Arc<Inflight>,
    metrics: Arc<DeliveryRuntimeMetrics>,
}

impl DeliveryRuntime {
    pub(super) fn new(config: BusRuntimeConfig) -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(config.delivery_queue_capacity());
        let metrics = Arc::new(DeliveryRuntimeMetrics::default());
        let inflight = Arc::new(Inflight::default());
        for worker_index in 0..config.delivery_workers() {
            spawn_delivery_worker(
                worker_index,
                receiver.clone(),
                Arc::clone(&metrics),
                Arc::clone(&inflight),
            );
        }
        Self {
            config,
            sender,
            inflight,
            metrics,
        }
    }

    pub(super) fn spawn_until(
        &self,
        deliveries: Vec<Delivery>,
        deadline: Instant,
        timeout_subject: String,
    ) -> Result<()> {
        self.inflight.add(deliveries.len());
        self.enqueue(deliveries, None, deadline, timeout_subject)
    }

    pub(super) fn run_and_wait_until(
        &self,
        deliveries: Vec<Delivery>,
        deadline: Instant,
        timeout_subject: String,
    ) -> Result<()> {
        if deliveries.is_empty() {
            return Ok(());
        }
        let expected = deliveries.len();
        self.inflight.add(expected);
        let (result_tx, result_rx) = mpsc::channel();
        self.enqueue(
            deliveries,
            Some(result_tx),
            deadline,
            timeout_subject.clone(),
        )?;

        let mut first_error = None;
        for _ in 0..expected {
            match result_rx.recv_timeout(remaining_until(deadline, timeout_subject.clone())?) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(BusError::Timeout {
                        subject: timeout_subject,
                    });
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(BusError::DeliveryRuntimeStopped);
                }
            }
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(super) fn snapshot(&self) -> BusRuntimeSnapshot {
        BusRuntimeSnapshot {
            delivery_workers: self.config.delivery_workers(),
            delivery_queue_capacity: self.config.delivery_queue_capacity(),
            max_active_deliveries: self.metrics.max_active_deliveries(),
            max_queued_deliveries: self.metrics.max_queued_deliveries(),
            enqueue_full_count: self.metrics.enqueue_full_count(),
            enqueue_blocked_ns: self.metrics.enqueue_blocked_ns(),
        }
    }

    pub(super) fn wait_for_idle(&self, deadline: Duration) -> Result<()> {
        self.inflight.wait_for_idle(deadline)
    }

    fn enqueue(
        &self,
        deliveries: Vec<Delivery>,
        result_tx: Option<mpsc::Sender<Result<()>>>,
        deadline: Instant,
        timeout_subject: String,
    ) -> Result<()> {
        let total = deliveries.len();
        for (queued, delivery) in deliveries.into_iter().enumerate() {
            let queue_len = self.sender.len();
            self.metrics.record_queue_depth(queue_len);
            let job = DeliveryJob {
                delivery,
                result_tx: result_tx.clone(),
            };
            let result = match self.sender.try_send(job) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(job)) => {
                    self.metrics
                        .record_queue_depth(self.config.delivery_queue_capacity());
                    let send_started = Instant::now();
                    let remaining = match remaining_until(deadline, timeout_subject.clone()) {
                        Ok(remaining) => remaining,
                        Err(error) => {
                            self.inflight.complete_many(total - queued);
                            return Err(error);
                        }
                    };
                    let result =
                        self.sender
                            .send_timeout(job, remaining)
                            .map_err(|error| match error {
                                SendTimeoutError::Timeout(_) => BusError::Timeout {
                                    subject: timeout_subject.clone(),
                                },
                                SendTimeoutError::Disconnected(_) => {
                                    BusError::DeliveryRuntimeStopped
                                }
                            });
                    self.metrics.record_enqueue_block(send_started.elapsed());
                    result
                }
                Err(TrySendError::Disconnected(_)) => Err(BusError::DeliveryRuntimeStopped),
            };
            if let Err(error) = result {
                self.inflight.complete_many(total - queued);
                return Err(error);
            }
            self.metrics.record_queue_depth(self.sender.len());
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Inflight {
    count: Mutex<usize>,
    idle: Condvar,
}

impl Inflight {
    fn add(&self, count: usize) {
        let mut in_flight = self.count.lock().expect("in-flight mutex poisoned");
        *in_flight += count;
    }

    fn complete(&self) {
        let mut in_flight = self.count.lock().expect("in-flight mutex poisoned");
        *in_flight = in_flight.saturating_sub(1);
        if *in_flight == 0 {
            self.idle.notify_all();
        }
    }

    fn complete_many(&self, count: usize) {
        let mut in_flight = self.count.lock().expect("in-flight mutex poisoned");
        *in_flight = in_flight.saturating_sub(count);
        if *in_flight == 0 {
            self.idle.notify_all();
        }
    }

    fn guard(self: &Arc<Self>) -> InflightGuard {
        InflightGuard {
            inflight: Arc::clone(self),
        }
    }

    fn wait_for_idle(&self, deadline: Duration) -> Result<()> {
        let started = Instant::now();
        let mut in_flight = self.count.lock().expect("in-flight mutex poisoned");
        while *in_flight > 0 {
            let Some(remaining) = deadline.checked_sub(started.elapsed()) else {
                return Err(BusError::Timeout {
                    subject: String::from("drain"),
                });
            };
            let (guard, wait_result) = self
                .idle
                .wait_timeout(in_flight, remaining)
                .expect("in-flight condvar wait poisoned");
            in_flight = guard;
            if wait_result.timed_out() && *in_flight > 0 {
                return Err(BusError::Timeout {
                    subject: String::from("drain"),
                });
            }
        }
        Ok(())
    }
}

struct InflightGuard {
    inflight: Arc<Inflight>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.inflight.complete();
    }
}

struct DeliveryJob {
    delivery: Delivery,
    result_tx: Option<mpsc::Sender<Result<()>>>,
}

#[derive(Debug, Default)]
struct DeliveryRuntimeMetrics {
    active_deliveries: AtomicUsize,
    max_active_deliveries: AtomicUsize,
    max_queued_deliveries: AtomicUsize,
    enqueue_full_count: AtomicUsize,
    enqueue_blocked_ns: AtomicU64,
}

impl DeliveryRuntimeMetrics {
    fn start_delivery(&self) -> ActiveDeliveryGuard<'_> {
        let active = self.active_deliveries.fetch_add(1, Ordering::SeqCst) + 1;
        self.record_max_active(active);
        ActiveDeliveryGuard { metrics: self }
    }

    fn record_max_active(&self, active: usize) {
        self.max_active_deliveries
            .fetch_max(active, Ordering::SeqCst);
    }

    fn finish_delivery(&self) {
        self.active_deliveries.fetch_sub(1, Ordering::SeqCst);
    }

    fn record_queue_depth(&self, queued: usize) {
        self.max_queued_deliveries
            .fetch_max(queued, Ordering::SeqCst);
    }

    fn record_enqueue_block(&self, duration: Duration) {
        self.enqueue_full_count.fetch_add(1, Ordering::SeqCst);
        let blocked_ns = duration_to_ns(duration);
        self.enqueue_blocked_ns
            .fetch_add(blocked_ns, Ordering::SeqCst);
    }

    fn max_active_deliveries(&self) -> usize {
        self.max_active_deliveries.load(Ordering::SeqCst)
    }

    fn max_queued_deliveries(&self) -> usize {
        self.max_queued_deliveries.load(Ordering::SeqCst)
    }

    fn enqueue_full_count(&self) -> usize {
        self.enqueue_full_count.load(Ordering::SeqCst)
    }

    fn enqueue_blocked_ns(&self) -> u64 {
        self.enqueue_blocked_ns.load(Ordering::SeqCst)
    }
}

struct ActiveDeliveryGuard<'a> {
    metrics: &'a DeliveryRuntimeMetrics,
}

impl Drop for ActiveDeliveryGuard<'_> {
    fn drop(&mut self) {
        self.metrics.finish_delivery();
    }
}

fn spawn_delivery_worker(
    worker_index: usize,
    receiver: DeliveryReceiver<DeliveryJob>,
    metrics: Arc<DeliveryRuntimeMetrics>,
    inflight: Arc<Inflight>,
) {
    thread::Builder::new()
        .name(format!("mvp-bus-delivery-{worker_index}"))
        .spawn(move || {
            while let Ok(job) = receiver.recv() {
                let _inflight_guard = inflight.guard();
                let _active_delivery = metrics.start_delivery();
                let result = job.delivery.invoke();
                if let Some(result_tx) = job.result_tx {
                    let _ = result_tx.send(result);
                }
            }
        })
        .expect("delivery worker starts");
}

fn duration_to_ns(duration: Duration) -> u64 {
    let nanos = duration.as_nanos();
    if nanos > u128::from(u64::MAX) {
        return u64::MAX;
    }
    nanos as u64
}
