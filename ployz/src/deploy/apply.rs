use std::{
    io::{self, IsTerminal, Write},
    num::NonZeroU32,
};

use ployz_core::{DeployEvent, ProjectName, RequestedServiceSpec, ServiceSelector};
use tokio_util::sync::CancellationToken;

use crate::{
    compose::{BuildService, ComposeProject},
    connect::Client,
    failure::Failure,
    project::ResolvedProject,
};

use super::{
    DeployOutcome, DeployPreview, ExecutionError, ServiceAttempt,
    pipeline::{
        PushOutcome, list_machines, plan_options, plan_project, plan_scale, plan_spec,
        push_project_images,
    },
    render,
};

pub(crate) async fn deploy_spec(
    client: &mut Client,
    requested: &RequestedServiceSpec,
    force_recreate: bool,
    skip_health_monitor: bool,
    project_name: &ProjectName,
    context: &str,
    project: Option<&ResolvedProject>,
) -> Result<(), Failure> {
    let preview = plan_spec(
        client,
        requested,
        plan_options(force_recreate, skip_health_monitor),
        project_name,
    )
    .await?;
    print_warnings(&preview);
    if preview.noop() {
        let source = project.map(|project| project.source.to_string());
        print!(
            "{}",
            render::plan_text(&preview, context, source.as_deref())
        );
        return Ok(());
    }
    finish(
        stream_confirm(
            client,
            &preview,
            format!("Running service {}", requested.name),
        )
        .await,
    )
}

pub(crate) async fn apply_requested(
    client: &mut Client,
    requested: &RequestedServiceSpec,
) -> Result<(), Failure> {
    deploy_spec(
        client,
        requested,
        false,
        false,
        &ProjectName::system(),
        "default",
        None,
    )
    .await
}

pub(crate) struct ConfirmGate<'a> {
    pub auto_confirm: bool,
    pub context: &'a str,
    pub project: &'a ResolvedProject,
}

pub(crate) async fn deploy_project(
    client: &mut Client,
    project: &mut ComposeProject,
    builds: &[BuildService],
    apply: Vec<ServiceAttempt>,
    force_recreate: bool,
    skip_health_monitor: bool,
    gate: ConfirmGate<'_>,
) -> Result<(), Failure> {
    let options = plan_options(force_recreate, skip_health_monitor);
    let machines = list_machines(client).await?;
    let outcome = push_project_images(client, builds, &machines).await?;
    print_pushed_images(&outcome);
    if !outcome.failures.is_empty() {
        return Err(Failure::usage(format!(
            "image push failed: {}",
            outcome.failures.join("; ")
        )));
    }
    let preview = plan_project(
        client,
        project,
        machines,
        apply,
        options,
        &gate.project.name,
    )
    .await?;
    print_warnings(&preview);
    confirm_and_execute(client, &preview, gate).await
}

pub(crate) async fn deploy_scale(
    client: &mut Client,
    selector: &ServiceSelector,
    replicas: NonZeroU32,
    skip_health_monitor: bool,
    gate: ConfirmGate<'_>,
) -> Result<(), Failure> {
    let (preview, project_name) = plan_scale(
        client,
        selector,
        replicas,
        plan_options(false, skip_health_monitor),
    )
    .await?;
    print_warnings(&preview);
    let project = ResolvedProject {
        name: project_name,
        source: gate.project.source,
    };
    confirm_and_execute(
        client,
        &preview,
        ConfirmGate {
            auto_confirm: gate.auto_confirm,
            context: gate.context,
            project: &project,
        },
    )
    .await
}

async fn confirm_and_execute(
    client: &Client,
    preview: &DeployPreview,
    gate: ConfirmGate<'_>,
) -> Result<(), Failure> {
    let source = Some(gate.project.source.to_string());
    print!(
        "{}",
        render::plan_text(preview, gate.context, source.as_deref())
    );
    if preview.noop() {
        return Ok(());
    }
    if !gate.auto_confirm && !confirm(gate.context)? {
        println!("No changes were made.");
        return Ok(());
    }
    finish(stream_confirm(client, preview, format!("Deploying to {}", gate.context)).await)
}

async fn stream_confirm(
    client: &Client,
    preview: &DeployPreview,
    title: String,
) -> DeployOutcome<ExecutionError> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let abort = cancel.clone();
    let ctrl_c = tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        abort.cancel();
    });
    let execute = client.confirm(preview, &cancel, Some(tx));
    tokio::pin!(execute);
    let mut printer = ProgressPrinter::new(title);
    let outcome = loop {
        tokio::select! {
            event = rx.recv() => {
                if let Some(event) = event {
                    printer.print(&event);
                }
            }
            outcome = &mut execute => {
                while let Ok(event) = rx.try_recv() {
                    printer.print(&event);
                }
                break outcome;
            }
        }
    };
    ctrl_c.abort();
    outcome
}

struct ProgressPrinter {
    title: String,
    last_lines: usize,
    last_signature: Option<String>,
}

impl ProgressPrinter {
    fn new(title: String) -> Self {
        Self {
            title,
            last_lines: 0,
            last_signature: None,
        }
    }

    fn print(&mut self, event: &DeployEvent) {
        let DeployEvent::Progress { .. } = event else {
            return;
        };
        let signature = progress_signature(event);
        let tty = io::stdout().is_terminal();
        if !tty && self.last_signature.as_ref() == Some(&signature) {
            return;
        }
        let text = render::progress_text(event, &self.title);
        if tty && self.last_lines > 0 {
            print!("\x1b[{}F\x1b[J", self.last_lines);
        }
        print!("{text}");
        let _ = io::stdout().flush();
        self.last_lines = text.lines().count();
        self.last_signature = Some(signature);
    }
}

fn progress_signature(event: &DeployEvent) -> String {
    match event {
        DeployEvent::Progress {
            rows, completed, ..
        } => {
            let kinds: Vec<_> = rows
                .iter()
                .map(|row| render::status_kind(&row.status))
                .collect();
            format!("{completed}:{}", kinds.join(","))
        }
        DeployEvent::Outcome { .. } => String::new(),
    }
}

fn print_pushed_images(outcome: &PushOutcome) {
    for pushed in &outcome.pushed {
        println!("Pushed {} to {}", pushed.image, pushed.machine_id);
    }
}

fn print_warnings(preview: &DeployPreview) {
    for warning in &preview.warnings {
        eprintln!("WARNING: {warning}");
    }
}

fn confirm(context: &str) -> Result<bool, Failure> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Failure::usage(
            "confirmation requires a terminal; pass --yes to continue",
        ));
    }
    print!("{}", render::confirm_prompt(context));
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn finish(outcome: DeployOutcome<ExecutionError>) -> Result<(), Failure> {
    let text = render::outcome_text(&outcome);
    if !text.is_empty() {
        print!("{text}");
    }
    match outcome {
        DeployOutcome::Success { .. } => Ok(()),
        DeployOutcome::Failed { .. } => Err(Failure::usage(text.trim().to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::DeployWarning;
    use crate::dns::ingress_dns_warnings;
    use ployz_core::RequestedServiceSpec;

    #[test]
    fn deploy_prints_ingress_misses_as_warning_lines_without_failing() {
        let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
            "name": "web",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "nginx", "pull_policy": "missing" },
            "ports": [
                {
                    "mode": "ingress",
                    "hostname": { "kind": "explicit", "hostname": "app.example.com" },
                    "load_balancer_port": 443,
                    "container_port": 8080,
                    "http_protocol": "https"
                },
                {
                    "mode": "ingress",
                    "hostname": { "kind": "explicit", "hostname": "plain.example.com" },
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
                "app.example.com" => vec!["198.51.100.10".parse().unwrap()],
                "plain.example.com" => Vec::new(),
                other => panic!("unexpected {other}"),
            })
            .into_iter()
            .map(DeployWarning::from)
            .collect(),
            ProjectName::parse("app").unwrap(),
        );
        assert_eq!(
            preview
                .warnings
                .iter()
                .map(|warning| format!("WARNING: {warning}"))
                .collect::<Vec<_>>(),
            [
                "WARNING: Ingress Hostname app.example.com resolves to 198.51.100.10; it should resolve to 192.0.2.1. A certificate cannot be issued until it points at this Cluster.",
                "WARNING: Ingress Hostname plain.example.com does not resolve; it should resolve to 192.0.2.1.",
            ]
        );
        assert!(
            !preview
                .warnings
                .iter()
                .map(|warning| format!("WARNING: {warning}"))
                .any(|line| line.contains("plain.example.com")
                    && line.to_ascii_lowercase().contains("certificate"))
        );
    }
}
