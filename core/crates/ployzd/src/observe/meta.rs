//! What the harvester and the sampler know of a container, made from an inspect.

use bollard::models::ContainerInspectResponse;
use ployz_core::labels;

use super::layout::{ContainerKind, ContainerMeta};

pub(super) fn container_meta(inspected: &ContainerInspectResponse) -> Option<ContainerMeta> {
    let labels = inspected.config.as_ref()?.labels.as_ref()?;
    let kind = ContainerKind::of(labels)?;
    let state = inspected.state.as_ref();
    let finished_at = state
        .and_then(|state| state.finished_at.clone())
        .filter(|at| !at.starts_with("0001-"));
    // A container Docker is restarting inspects as running, yet it has exited.
    let ended = finished_at.is_some()
        && (state.and_then(|state| state.running) != Some(true)
            || state.and_then(|state| state.restarting) == Some(true));
    Some(ContainerMeta {
        service: labels.get(labels::SERVICE_NAME).cloned(),
        namespace: labels.get(labels::NAMESPACE).cloned(),
        deployment: labels.get(labels::DEPLOYMENT_ID).cloned(),
        replica: inspected
            .name
            .as_deref()
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_owned(),
        kind,
        started_at: state.and_then(|state| state.started_at.clone()),
        finished_at: finished_at.filter(|_| ended),
        exit_code: state.and_then(|state| state.exit_code).filter(|_| ended),
        oom_killed: state.and_then(|state| state.oom_killed).unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::container_meta;
    use crate::observe::{fixtures::labelled, layout::ContainerKind};

    #[test]
    fn ingress_is_a_system_container_and_an_app_is_a_service() {
        let kind = |namespace: &str, service: &str| {
            container_meta(&labelled(&[
                ("ployz.managed", "true"),
                ("ployz.namespace", namespace),
                ("ployz.service.name", service),
            ]))
            .unwrap()
            .kind
        };
        assert_eq!(kind("ployz-system", "ingress"), ContainerKind::System);
        assert_eq!(kind("default", "web"), ContainerKind::Service);
    }

    #[test]
    fn container_meta_classifies_by_labels_leniently() {
        let classify = |labels: &[(&str, &str)]| {
            container_meta(&labelled(labels)).map(|meta| {
                (
                    meta.kind,
                    meta.namespace,
                    meta.service,
                    meta.deployment,
                    meta.replica,
                )
            })
        };
        let owned =
            |kind, namespace: Option<&str>, service: Option<&str>, deployment: Option<&str>| {
                Some((
                    kind,
                    namespace.map(str::to_owned),
                    service.map(str::to_owned),
                    deployment.map(str::to_owned),
                    "replica".to_owned(),
                ))
            };
        assert_eq!(classify(&[]), None);
        assert_eq!(classify(&[("com.example.other", "x")]), None);
        assert_eq!(classify(&[("ployz.namespace", "app")]), None);
        assert_eq!(
            classify(&[("ployz.service.name", "web"), ("ployz.service.id", "x")]),
            None
        );
        assert_eq!(
            classify(&[("ployzd.managed", "")]),
            owned(ContainerKind::System, None, None, None)
        );
        assert_eq!(
            classify(&[("ployz.managed", "")]),
            owned(ContainerKind::System, None, None, None),
            "a managed container with no Service name is a system one"
        );
        assert_eq!(
            classify(&[
                ("ployz.managed", ""),
                ("ployz.namespace", "app"),
                ("ployz.service.name", "web"),
                ("ployz.deployment.id", "d1"),
            ]),
            owned(ContainerKind::Service, Some("app"), Some("web"), Some("d1"))
        );
        assert_eq!(
            classify(&[
                ("ployz.managed", ""),
                ("ployz.namespace", "ployz-system"),
                ("ployz.service.name", "ingress"),
            ]),
            owned(
                ContainerKind::System,
                Some("ployz-system"),
                Some("ingress"),
                None
            )
        );
        assert_eq!(
            classify(&[
                ("ployz.managed", ""),
                ("ployz.namespace", "ployz-system"),
                ("ployz.service.name", "ingress"),
                ("ployz.service.hook", "pre-deploy"),
            ]),
            owned(
                ContainerKind::PreDeployHook,
                Some("ployz-system"),
                Some("ingress"),
                None
            ),
            "a hook label wins over the system Namespace"
        );
        assert_eq!(
            classify(&[
                ("ployz.managed", ""),
                ("ployz.service.name", "web"),
                ("ployz.service.hook", ""),
            ]),
            owned(ContainerKind::PreDeployHook, None, Some("web"), None),
            "any hook value is a hook"
        );
        assert_eq!(
            classify(&[
                ("ployzd.managed", ""),
                ("ployz.service.name", "web"),
                ("ployz.namespace", "app"),
            ]),
            owned(ContainerKind::Service, Some("app"), Some("web"), None),
            "the system marker admits a container the Service labels then classify"
        );
        assert_eq!(
            classify(&[("ployz.service.hook", "pre-deploy")]),
            None,
            "a hook label alone does not make a container Ployz's"
        );
    }
}
