//! Direct infrastructure execution and its presentation lifetime.

use super::{DeployError, pipeline::plan_options, render, report};
use crate::{
    cancellation::CtrlC,
    connect::Client,
    failure::Failure,
    ui::{
        self, Hint,
        progress::{Disposition, Progress},
    },
};
use ployz_core::{
    DeployEvent, DeployIntent, DeployOutcome, ExecutionError, Namespace, RequestedServiceSpec,
};

/// Apply one Ployz infrastructure Service in the reserved system Namespace.
pub(crate) async fn apply_requested(
    client: &mut Client,
    requested: &RequestedServiceSpec,
    force_recreate: bool,
    skip_health_monitor: bool,
    context: &str,
) -> Result<Outcome, ApplyError> {
    let signal = CtrlC::subscribe().map_err(Failure::from)?;
    let prepare = crate::setup_retry::run(
        client,
        "Preparing service deployment",
        crate::setup_retry::WAIT,
        |error| matches!(error, DeployError::Connect(error) if error.is_setup_retryable()),
        async |client| {
            client
                .preview(DeployIntent::apply_one(
                    Namespace::system(),
                    requested.clone(),
                    plan_options(force_recreate, skip_health_monitor),
                ))
                .await
        },
    );
    let preview = tokio::select! {
        biased;
        () = signal.token().cancelled() => return Err(Failure::cancelled().into()),
        result = prepare => result.map_err(Failure::from)?,
    };
    if preview.noop() {
        for warning in &preview.warnings {
            ui::warn(warning.to_string());
        }
        ui::note("Everything is up to date.");
        return Ok(DeployOutcome::Success {
            completed: Vec::new(),
        });
    }
    for warning in &preview.warnings {
        ui::warn(warning.to_string());
    }
    let mut evidence = report::Direct::new(
        &preview,
        format!("Deploying {} to {context}", requested.name),
    );
    let mut progress = Progress::start(evidence.frame());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let confirm = client.confirm(&preview, signal.token(), Some(tx));
    tokio::pin!(confirm);
    let outcome = loop {
        tokio::select! {
            event = rx.recv(), if !rx.is_closed() || !rx.is_empty() => if let Some(DeployEvent::Progress { rows, .. }) = event {
                evidence.observe(&rows);
                progress.update(evidence.frame());
            },
            outcome = &mut confirm => {
                while let Ok(event) = rx.try_recv() {
                    if let DeployEvent::Progress { rows, .. } = event { evidence.observe(&rows); progress.update(evidence.frame()); }
                }
                break outcome;
            }
        }
    };
    let tails = evidence.tails(client).await;
    progress.finish(
        evidence.frame(),
        if signal.token().is_cancelled() {
            Disposition::LocalStopped
        } else {
            Disposition::Settled
        },
    );
    match outcome {
        DeployOutcome::Success { ref completed } => {
            ui::note_inline(format_args!("{}", render::endpoints_footer(completed)));
            if signal.token().is_cancelled() {
                Err(Failure::cancelled().into())
            } else {
                Ok(outcome)
            }
        }
        DeployOutcome::Failed { .. } => {
            let mut failure =
                report::failure(&outcome, tails, context).hint(Hint::Retry(shell_words::join([
                    "ployz",
                    "server",
                    "set",
                    &preview
                        .operations
                        .first()
                        .expect("nonempty preview")
                        .machine_id
                        .to_string(),
                    "--accepts-ingress=true",
                    "--context",
                    context,
                ])));
            if signal.token().is_cancelled() {
                failure = failure.interrupted();
            }
            Err(failure.into())
        }
    }
}

/// A direct follow-up failure retains diagnostic evidence until the outer command adds context.
#[derive(Debug)]
pub(crate) struct ApplyError(Failure);
impl From<Failure> for ApplyError {
    fn from(failure: Failure) -> Self {
        Self(failure)
    }
}
impl From<DeployError> for ApplyError {
    fn from(error: DeployError) -> Self {
        Self(error.into())
    }
}
impl From<ApplyError> for Failure {
    fn from(error: ApplyError) -> Self {
        error.0
    }
}

/// Terminal evidence of a Deploy that ran to completion.
pub(crate) type Outcome = DeployOutcome<ExecutionError>;
#[cfg(test)]
mod tests {
    use super::super::pipeline::namespace_not_found;
    use super::*;
    use crate::deploy::DeployWarning;
    use crate::dns::ingress_dns_warnings;
    use ployz_core::DeployPreview;
    use ployz_core::{PruneRefusal, RequestedServiceSpec};

    #[test]
    fn deploy_prints_ingress_misses_as_warning_lines_without_failing() {
        let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
            "name": "web",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "nginx", "pull_policy": "missing" },
            "ports": [
                {
                    "mode": "ingress",
                    "hostname": "app.example.com",
                    "load_balancer_port": 443,
                    "container_port": 8080,
                    "http_protocol": "https"
                },
                {
                    "mode": "ingress",
                    "hostname": "plain.example.com",
                    "load_balancer_port": 80,
                    "container_port": 8080,
                    "http_protocol": "http"
                }
            ]
        }))
        .unwrap();
        let cluster = ["192.0.2.1".parse().unwrap()];
        let preview = DeployPreview::new(
            Vec::new(),
            ingress_dns_warnings([&spec], &cluster, |hostname| match hostname.as_str() {
                "app.example.com" => {
                    ployz_core::HostnameVerdict::Refused(ployz_core::Refusal::ReachesElsewhere)
                }
                "plain.example.com" => {
                    ployz_core::HostnameVerdict::Refused(ployz_core::Refusal::DoesNotResolve)
                }
                other => panic!("unexpected {other}"),
            })
            .into_iter()
            .map(DeployWarning::from)
            .collect(),
            Namespace::parse("app").unwrap(),
        );
        assert_eq!(
            preview
                .warnings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            [
                "app.example.com answers from another server. Point it at 192.0.2.1. A certificate cannot be issued until then.",
                "plain.example.com does not resolve. Add a DNS record pointing at 192.0.2.1.",
            ]
        );
        assert!(
            !preview
                .warnings
                .iter()
                .map(ToString::to_string)
                .any(|line| line.contains("plain.example.com")
                    && line.to_ascii_lowercase().contains("certificate"))
        );
    }

    #[test]
    fn incomplete_empty_view_is_not_reported_as_missing() {
        let mut preview =
            DeployPreview::new(Vec::new(), Vec::new(), Namespace::parse("shop").unwrap());
        assert!(namespace_not_found(&preview));
        preview.prune_refusal = Some(PruneRefusal::IncompleteSnapshot);
        assert!(!namespace_not_found(&preview));
    }
}
