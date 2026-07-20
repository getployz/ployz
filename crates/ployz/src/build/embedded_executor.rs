use std::future::Future;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::PloyzctlExecutionOutput;

pub(crate) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) async fn run_once<Executor, Operation, OperationFuture, Cancellation, T>(
    executor: Executor,
    ready: oneshot::Receiver<()>,
    shutdown: oneshot::Sender<()>,
    operation: Operation,
    cancellation: Cancellation,
) -> Result<T, PloyzctlExecutionError>
where
    Executor:
        Future<Output = Result<PloyzctlExecutionOutput, PloyzctlExecutionError>> + Send + 'static,
    Operation: FnOnce() -> OperationFuture,
    OperationFuture: Future<Output = Result<T, PloyzctlExecutionError>>,
    Cancellation: Future<Output = Result<(), std::io::Error>>,
{
    let mut task = tokio::spawn(executor);
    tokio::pin!(cancellation);

    let ready_result = tokio::select! {
        biased;
        ready = ready => ready.map_err(current_tree_error),
        joined = &mut task => return joined_before_ready(joined),
        cancelled = &mut cancellation => cancellation_error(cancelled),
    };
    if let Err(error) = ready_result {
        return finish(Err(error), shutdown, task).await;
    }

    let operation = operation();
    tokio::pin!(operation);
    let (result, joined) = tokio::select! {
        result = &mut operation => (result, false),
        executor = &mut task => {
            match executor {
                Ok(Ok(_)) => {
                    let result = tokio::select! {
                        result = &mut operation => result,
                        cancelled = &mut cancellation => cancellation_error(cancelled),
                    };
                    (result, true)
                }
                Ok(Err(error)) => (Err(error), true),
                Err(error) => (Err(current_tree_error(error)), true),
            }
        }
        cancelled = &mut cancellation => (cancellation_error(cancelled), false),
    };
    if joined {
        result
    } else {
        finish(result, shutdown, task).await
    }
}

fn joined_before_ready<T>(
    joined: Result<Result<PloyzctlExecutionOutput, PloyzctlExecutionError>, tokio::task::JoinError>,
) -> Result<T, PloyzctlExecutionError> {
    match joined {
        Ok(Ok(_)) => Err(current_tree_error(
            "build executor exited before becoming ready",
        )),
        Ok(Err(error)) => Err(error),
        Err(error) => Err(current_tree_error(error)),
    }
}

fn cancellation_error<T>(signal: Result<(), std::io::Error>) -> Result<T, PloyzctlExecutionError> {
    match signal {
        Ok(()) => Err(current_tree_error("current-tree deploy cancelled")),
        Err(error) => Err(current_tree_error(error)),
    }
}

async fn finish<T>(
    result: Result<T, PloyzctlExecutionError>,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<Result<PloyzctlExecutionOutput, PloyzctlExecutionError>>,
) -> Result<T, PloyzctlExecutionError> {
    match shutdown_executor(shutdown, task).await {
        Ok(()) => result,
        Err(error) => Err(error),
    }
}

async fn shutdown_executor(
    shutdown: oneshot::Sender<()>,
    mut task: JoinHandle<Result<PloyzctlExecutionOutput, PloyzctlExecutionError>>,
) -> Result<(), PloyzctlExecutionError> {
    let _ = shutdown.send(());
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut task).await {
        Ok(joined) => {
            let _ = joined.map_err(current_tree_error)??;
            Ok(())
        }
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(current_tree_error("build executor shutdown timed out"))
        }
    }
}

fn current_tree_error(message: impl std::fmt::Display) -> PloyzctlExecutionError {
    crate::deploy::runtime::DeployExecutionError::CurrentTree {
        message: message.to_string(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> PloyzctlExecutionOutput {
        PloyzctlExecutionOutput::stdout(String::new())
    }

    fn pending_cancel() -> impl Future<Output = Result<(), std::io::Error>> {
        std::future::pending()
    }

    #[tokio::test]
    async fn successful_exit_before_readiness_fails() {
        let (_ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let result = run_once(
            async { Ok(output()) },
            ready_rx,
            shutdown_tx,
            || async { Ok::<_, PloyzctlExecutionError>(()) },
            pending_cancel(),
        )
        .await;
        assert!(
            result
                .expect_err("pre-ready exit must fail")
                .to_string()
                .contains("before becoming ready")
        );
    }

    #[tokio::test]
    async fn successful_once_exit_waits_for_durable_operation() {
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let (operation_tx, operation_rx) = oneshot::channel();
        let executor = async move {
            let _ = ready_tx.send(());
            Ok(output())
        };
        let result = tokio::spawn(run_once(
            executor,
            ready_rx,
            shutdown_tx,
            || async move { operation_rx.await.map_err(current_tree_error) },
            pending_cancel(),
        ));
        tokio::task::yield_now().await;
        assert!(!result.is_finished());
        operation_tx.send("terminal").expect("operation receiver");
        assert_eq!(result.await.expect("join").expect("result"), "terminal");
    }

    #[tokio::test]
    async fn already_delivered_readiness_wins_over_simultaneous_successful_exit() {
        for _ in 0..100 {
            let (ready_tx, ready_rx) = oneshot::channel();
            ready_tx.send(()).expect("ready receiver");
            let (shutdown_tx, _shutdown_rx) = oneshot::channel();
            let result = run_once(
                async { Ok(output()) },
                ready_rx,
                shutdown_tx,
                || async { Ok::<_, PloyzctlExecutionError>("terminal") },
                pending_cancel(),
            )
            .await;
            assert_eq!(result.expect("post-ready success"), "terminal");
        }
    }

    #[tokio::test]
    async fn executor_error_after_readiness_aborts_operation() {
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let executor = async move {
            let _ = ready_tx.send(());
            Err(current_tree_error("executor failed"))
        };
        let error = run_once(
            executor,
            ready_rx,
            shutdown_tx,
            std::future::pending::<Result<(), PloyzctlExecutionError>>,
            pending_cancel(),
        )
        .await
        .expect_err("executor failure");
        assert!(error.to_string().contains("executor failed"));
    }

    #[tokio::test]
    async fn cancellation_remains_live_after_successful_once_exit() {
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let executor = async move {
            let _ = ready_tx.send(());
            Ok(output())
        };
        let result = tokio::spawn(run_once(
            executor,
            ready_rx,
            shutdown_tx,
            std::future::pending::<Result<(), PloyzctlExecutionError>>,
            async move { cancel_rx.await.map_err(std::io::Error::other) },
        ));
        tokio::task::yield_now().await;
        assert!(!result.is_finished());
        cancel_tx.send(()).expect("cancel receiver");
        let error = result
            .await
            .expect("join")
            .expect_err("cancellation remains live");
        assert!(error.to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn cancellation_stops_a_ready_executor() {
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let executor = async move {
            let _ = ready_tx.send(());
            let _ = shutdown_rx.await;
            Ok(output())
        };
        let error = run_once(
            executor,
            ready_rx,
            shutdown_tx,
            std::future::pending::<Result<(), PloyzctlExecutionError>>,
            async { Ok(()) },
        )
        .await
        .expect_err("cancelled");
        assert!(error.to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn cancellation_before_readiness_stops_the_executor() {
        let (_ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (stopped_tx, stopped_rx) = oneshot::channel();
        let executor = async move {
            let _ = shutdown_rx.await;
            let _ = stopped_tx.send(());
            Ok(output())
        };
        let error = run_once(
            executor,
            ready_rx,
            shutdown_tx,
            || async { Ok::<_, PloyzctlExecutionError>(()) },
            async { Ok(()) },
        )
        .await
        .expect_err("cancelled");
        assert!(error.to_string().contains("cancelled"));
        stopped_rx.await.expect("executor shut down");
    }

    #[tokio::test]
    async fn shutdown_failure_overrides_operation_success() {
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let executor = async move {
            let _ = ready_tx.send(());
            let _ = shutdown_rx.await;
            Err(current_tree_error("cleanup failed"))
        };
        let error = run_once(
            executor,
            ready_rx,
            shutdown_tx,
            || async { Ok::<_, PloyzctlExecutionError>(()) },
            pending_cancel(),
        )
        .await
        .expect_err("shutdown failure");
        assert!(error.to_string().contains("cleanup failed"));
    }
}
