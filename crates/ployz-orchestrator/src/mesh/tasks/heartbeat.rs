use crate::mesh::tasks::{ParticipationCommand, SelfLivenessCommand};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::info;

#[derive(Debug)]
pub enum HeartbeatCommand {
    TickNow { done: oneshot::Sender<()> },
}

pub(crate) async fn run_heartbeat_task(
    self_liveness_tx: mpsc::Sender<SelfLivenessCommand>,
    participation_tx: mpsc::Sender<ParticipationCommand>,
    mut commands: mpsc::Receiver<HeartbeatCommand>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("heartbeat coordinator task cancelled");
                break;
            }
            Some(command) = commands.recv() => {
                match command {
                    HeartbeatCommand::TickNow { done } => {
                        let _ = tokio::join!(
                            tick_self_liveness(&self_liveness_tx),
                            tick_participation(&participation_tx),
                        );
                        let _ = done.send(());
                    }
                }
            }
        }
    }
}

async fn tick_self_liveness(
    self_liveness_tx: &mpsc::Sender<SelfLivenessCommand>,
) -> Result<(), ()> {
    let (done_tx, done_rx) = oneshot::channel();
    self_liveness_tx
        .send(SelfLivenessCommand::TickNow { done: done_tx })
        .await
        .map_err(|_| ())?;
    done_rx.await.map_err(|_| ())
}

async fn tick_participation(
    participation_tx: &mpsc::Sender<ParticipationCommand>,
) -> Result<(), ()> {
    let (done_tx, done_rx) = oneshot::channel();
    participation_tx
        .send(ParticipationCommand::TickNow { done: done_tx })
        .await
        .map_err(|_| ())?;
    done_rx.await.map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn heartbeat_tick_now_runs_liveness_then_participation() {
        let (self_liveness_tx, mut self_liveness_rx) = mpsc::channel(1);
        let (participation_tx, mut participation_rx) = mpsc::channel(1);
        let (command_tx, command_rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            run_heartbeat_task(self_liveness_tx, participation_tx, command_rx, task_cancel).await;
        });

        let liveness_ack = tokio::spawn(async move {
            let Some(SelfLivenessCommand::TickNow { done }) = self_liveness_rx.recv().await else {
                panic!("expected liveness tick");
            };
            done.send(()).expect("ack liveness");
        });
        let participation_ack = tokio::spawn(async move {
            let Some(ParticipationCommand::TickNow { done }) = participation_rx.recv().await else {
                panic!("expected participation tick");
            };
            done.send(()).expect("ack participation");
        });

        let (done_tx, done_rx) = oneshot::channel();
        command_tx
            .send(HeartbeatCommand::TickNow { done: done_tx })
            .await
            .expect("send heartbeat tick");
        done_rx.await.expect("heartbeat ack");

        cancel.cancel();
        handle.await.expect("heartbeat task exits");
        liveness_ack.await.expect("liveness ack task");
        participation_ack.await.expect("participation ack task");
    }
}
