//! Rung 3: a cleanup no proxy confirms ends with the old copy left running, named by copy and Machine.

use super::support::*;
use ployz::deploy::{DeployEvent, DeployIntent, DeployOutcome, DeployWarning};
use ployz_core::{ContainerId, CopyRotation, Namespace, RequestedServiceSpec};
use tokio_util::sync::CancellationToken;

fn image(name: &str, image: &str) -> RequestedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": image, "pull_policy": "always" }
    }))
    .unwrap()
}

async fn replace_with_a_sticky_proxy(
    cancel_once_out: bool,
) -> (
    DeployOutcome<ployz::deploy::ExecutionError>,
    Vec<DeployWarning>,
    ContainerId,
) {
    let (one, two) = (machine('a', "one"), machine('b', "two"));
    let mut old = running_container(&one, &image("web", "web:1"));
    old.try_update(|parts| parts.rotation = Some(CopyRotation::In))
        .unwrap();
    let old_id = old.container_id;
    let service = DeployService::new(one.clone())
        .with_machines(vec![one, two.clone()])
        .never_unroute_on(two.machine.id);
    service.listed_containers().lock().unwrap().push(old);
    let unrouted = service.unrouted();
    let (mut client, server) = connected_in_memory(service).await;
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let run = client.run(
        DeployIntent::apply_one(
            Namespace::parse("app").unwrap(),
            image("web", "web:2"),
            skip_health(),
        ),
        &cancel,
        Some(tx),
    );
    tokio::pin!(run);
    let mut warnings = Vec::new();
    let outcome = loop {
        tokio::select! {
            outcome = &mut run => break outcome.unwrap(),
            Some(event) = rx.recv() => if let DeployEvent::Warning { warning } = event {
                warnings.push(warning);
            },
            () = tokio::time::sleep(std::time::Duration::from_millis(100)),
                if cancel_once_out && !cancel.is_cancelled() =>
            {
                if unrouted.lock().unwrap().contains(&old_id) {
                    cancel.cancel();
                }
            }
        }
    };
    while let Ok(event) = rx.try_recv() {
        if let DeployEvent::Warning { warning } = event {
            warnings.push(warning);
        }
    }
    server.abort();
    (outcome, warnings, old_id)
}

fn left_running(warnings: &[DeployWarning]) -> Vec<String> {
    warnings
        .iter()
        .filter(|warning| matches!(warning, DeployWarning::LeftRunning { .. }))
        .map(ToString::to_string)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn report_names_left_running() {
    let (outcome, warnings, old) = replace_with_a_sticky_proxy(false).await;

    assert!(
        !matches!(outcome, DeployOutcome::Success { .. }),
        "an Out no proxy confirms is no success: {outcome:?}"
    );
    assert_eq!(
        left_running(&warnings),
        [format!(
            "app/web copy {} left running on one (proxy on two did not confirm). The next deploy cleans it up.",
            &old.as_str()[..12]
        )],
        "{warnings:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn interrupting_cleanup_names_the_copy_left_running() {
    let (_, warnings, old) = replace_with_a_sticky_proxy(true).await;

    assert_eq!(
        left_running(&warnings),
        [format!(
            "app/web copy {} left running on one (interrupted). The next deploy cleans it up.",
            &old.as_str()[..12]
        )],
        "{warnings:?}"
    );
}
