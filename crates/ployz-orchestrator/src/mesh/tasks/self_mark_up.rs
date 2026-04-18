use super::SelfRecordMutation;
use super::self_record::SelfRecordCommand;
use crate::model::{MachineStatus, OverlayIp};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

const INITIAL_BACKOFF: Duration = Duration::from_millis(200);
const MAX_BACKOFF: Duration = Duration::from_secs(10);

pub(crate) async fn run_self_mark_up_task(
    self_record_tx: mpsc::Sender<SelfRecordCommand>,
    bridge_ip: Option<OverlayIp>,
    now_unix_secs: fn() -> u64,
    cancel: CancellationToken,
) {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        let result = super::apply_self_record_mutation(
            &self_record_tx,
            SelfRecordMutation::MarkUp {
                now: now_unix_secs(),
                bridge_ip,
            },
        )
        .await;
        match result {
            Some(record) if record.status == MachineStatus::Up => return,
            Some(_) => {
                warn!("self mark-up produced non-Up record; retrying");
            }
            None => {
                warn!("self mark-up failed; retrying");
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => {
                info!("self mark-up task cancelled before success");
                return;
            }
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineId, MachineRecord, PublicKey};
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;

    fn fixed_now() -> u64 {
        42
    }

    fn up_record() -> MachineRecord {
        MachineRecord {
            id: MachineId("self".into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            status: MachineStatus::Up,
            drain: false,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn succeeds_on_first_attempt() {
        let (tx, mut rx) = mpsc::channel::<SelfRecordCommand>(4);
        let cancel = CancellationToken::new();

        let responder = tokio::spawn(async move {
            let command = rx.recv().await.expect("command received");
            let _ = command.done.send(Some(up_record()));
        });

        run_self_mark_up_task(tx, None, fixed_now, cancel).await;
        responder.await.expect("responder exits");
    }

    #[tokio::test]
    async fn retries_after_failed_writes() {
        let (tx, mut rx) = mpsc::channel::<SelfRecordCommand>(4);
        let cancel = CancellationToken::new();

        let responder = tokio::spawn(async move {
            let first = rx.recv().await.expect("first command");
            let _ = first.done.send(None);
            let second = rx.recv().await.expect("second command");
            let _ = second.done.send(None);
            let third = rx.recv().await.expect("third command");
            let _ = third.done.send(Some(up_record()));
        });

        run_self_mark_up_task(tx, None, fixed_now, cancel).await;
        responder.await.expect("responder exits");
    }

    #[tokio::test]
    async fn cancels_during_backoff() {
        let (tx, mut rx) = mpsc::channel::<SelfRecordCommand>(4);
        let cancel = CancellationToken::new();
        let cancel_inner = cancel.clone();

        let responder = tokio::spawn(async move {
            let command = rx.recv().await.expect("command received");
            let _ = command.done.send(None);
            cancel_inner.cancel();
        });

        run_self_mark_up_task(tx, None, fixed_now, cancel).await;
        responder.await.expect("responder exits");
    }
}
