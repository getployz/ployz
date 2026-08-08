//! Route Binding primitives and refusal presentation.

use hyper::Method;
use ployz_core::corrosion::IngressMode;
use ployz_core::{
    ROUTE_ATTACH_ROUTE, RouteAttachOutcome, RouteAttachRefusal, RouteAttachReply,
    RouteAttachRequest,
};

use crate::commands::{RouteAttachCommand, RouteCommand, SshTarget};
use crate::init::ssh::shell_quote;
use crate::mesh::http::JsonReply;
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: RouteCommand) -> Result<String, RouteExecutionError> {
    match command {
        RouteCommand::Attach(command) => attach(command).await,
        RouteCommand::Remove(command) => crate::removal::execute_route_remove(command)
            .await
            .map_err(RouteExecutionError::Remove),
    }
}

async fn attach(command: RouteAttachCommand) -> Result<String, RouteExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let target = command.target.clone();
    let hostname = command.hostname.as_str().to_owned();
    let request = RouteAttachRequest {
        hostname: command.hostname,
        namespace_name: command.namespace,
        namespace_id: command.namespace_id,
        service_name: command.service,
        service_id: command.service_id,
        endpoint_port: command.endpoint_port,
        ingress_mode: IngressMode::Direct,
    };
    let reply = remote
        .request_json_with_refusal::<_, RouteAttachReply, RouteAttachRefusal>(
            Method::POST,
            ROUTE_ATTACH_ROUTE,
            Some(&request),
        )
        .await?;
    match reply {
        JsonReply::Success(RouteAttachReply {
            route_id,
            outcome: RouteAttachOutcome::Attached,
        }) => Ok(format!("attached route {hostname} ({route_id})\n")),
        JsonReply::Success(RouteAttachReply {
            route_id,
            outcome: RouteAttachOutcome::AlreadyAttached,
        }) => Ok(format!(
            "route {hostname} ({route_id}) is already attached\n"
        )),
        JsonReply::Refused(refusal) => Err(RouteExecutionError::Refused {
            message: refusal_message(refusal, &request, target.as_ref()),
        }),
    }
}

fn refusal_message(
    refusal: RouteAttachRefusal,
    request: &RouteAttachRequest,
    target: Option<&SshTarget>,
) -> String {
    match refusal {
        RouteAttachRefusal::UnsupportedIngressMode { requested } => format!(
            "ingress mode {requested:?} is not shipped; retry with direct ingress using `{}`",
            attach_command(
                request,
                request.namespace_id.as_ref(),
                request.service_id.as_ref(),
                target,
            )
        ),
        RouteAttachRefusal::NamespaceNotFound { namespace_name } => format!(
            "namespace {} does not exist; create it with `{}`, then retry `{}`",
            namespace_name.as_str(),
            namespace_create_command(&namespace_name, target),
            attach_command(
                request,
                request.namespace_id.as_ref(),
                request.service_id.as_ref(),
                target,
            )
        ),
        RouteAttachRefusal::NamespaceAmbiguous {
            namespace_name,
            namespace_ids,
        } => format!(
            "namespace {} is ambiguous; choose an exact row: {}",
            namespace_name.as_str(),
            namespace_ids
                .iter()
                .map(|namespace_id| format!(
                    "`{}`",
                    attach_command(
                        request,
                        Some(namespace_id),
                        request.service_id.as_ref(),
                        target,
                    )
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        RouteAttachRefusal::NamespaceIdMismatch {
            namespace_name,
            requested,
            found,
        } => format!(
            "namespace row {} is absent, but {} is row {}; retry `{}`",
            requested.as_str(),
            namespace_name.as_str(),
            found.as_str(),
            attach_command(request, Some(&found), request.service_id.as_ref(), target)
        ),
        RouteAttachRefusal::NamespaceIdentityMismatch {
            namespace_id,
            requested_name,
            found_name,
        } => format!(
            "namespace row {} is named {}, not {}; retry name selection with `{}`",
            namespace_id.as_str(),
            found_name.as_str(),
            requested_name.as_str(),
            attach_command(request, None, request.service_id.as_ref(), target)
        ),
        RouteAttachRefusal::NamespaceStoredRowUnselectable { namespace_id } => format!(
            "namespace row {} exists but this binary cannot safely select it; retry name selection with `{}`",
            namespace_id.as_str(),
            attach_command(request, None, request.service_id.as_ref(), target)
        ),
        RouteAttachRefusal::ServiceNotFound {
            namespace_id,
            service_name,
        } => format!(
            "service {} does not exist in namespace row {}; create it with the `ployz deploy` primitive (which requires an IMAGE), then retry `{}`",
            service_name.as_str(),
            namespace_id.as_str(),
            attach_command(request, Some(&namespace_id), None, target)
        ),
        RouteAttachRefusal::ServiceAmbiguous {
            namespace_id,
            service_name,
            service_ids,
        } => format!(
            "service {} in namespace row {} is ambiguous; choose an exact row: {}",
            service_name.as_str(),
            namespace_id.as_str(),
            service_ids
                .iter()
                .map(|service_id| format!(
                    "`{}`",
                    attach_command(request, Some(&namespace_id), Some(service_id), target)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        RouteAttachRefusal::ServiceIdMismatch {
            namespace_id,
            service_name,
            requested,
            found,
        } => format!(
            "service row {} is absent, but {} in namespace row {} is row {}; retry `{}`",
            requested.as_str(),
            service_name.as_str(),
            namespace_id.as_str(),
            found.as_str(),
            attach_command(request, Some(&namespace_id), Some(&found), target)
        ),
        RouteAttachRefusal::ServiceIdentityMismatch {
            service_id,
            requested_namespace_id,
            requested_name,
            found_namespace_id,
            found_name,
        } => format!(
            "service row {} is {}/{}, not {}/{}; retry name selection with `{}`",
            service_id.as_str(),
            found_namespace_id.as_str(),
            found_name.as_str(),
            requested_namespace_id.as_str(),
            requested_name.as_str(),
            attach_command(request, request.namespace_id.as_ref(), None, target)
        ),
        RouteAttachRefusal::ServiceStoredRowUnselectable { service_id } => format!(
            "service row {} exists but this binary cannot safely select it; retry name selection with `{}`",
            service_id.as_str(),
            attach_command(request, request.namespace_id.as_ref(), None, target)
        ),
        RouteAttachRefusal::HostnameAlreadyAttached {
            hostname,
            route_id,
            remove,
        } => format!(
            "hostname {} is already attached by route {}; remove it first with `ployz route rm {} --id {}{}`",
            hostname.as_str(),
            route_id.as_str(),
            shell_quote(remove.hostname.as_str()),
            shell_quote(
                remove
                    .route_id
                    .as_ref()
                    .map_or(route_id.as_str(), |id| id.as_str())
            ),
            target_suffix(target),
        ),
    }
}

fn attach_command(
    request: &RouteAttachRequest,
    namespace_id: Option<&ployz_core::ids::NamespaceRowId>,
    service_id: Option<&ployz_core::ids::ServiceRowId>,
    target: Option<&SshTarget>,
) -> String {
    let mut command = format!(
        "ployz route attach {} --namespace {} --service {} --port {}",
        shell_quote(request.hostname.as_str()),
        shell_quote(request.namespace_name.as_str()),
        shell_quote(request.service_name.as_str()),
        shell_quote(&request.endpoint_port.get().to_string()),
    );
    if let Some(namespace_id) = namespace_id {
        command.push_str(" --namespace-id ");
        command.push_str(&shell_quote(namespace_id.as_str()));
    }
    if let Some(service_id) = service_id {
        command.push_str(" --service-id ");
        command.push_str(&shell_quote(service_id.as_str()));
    }
    command.push_str(&target_suffix(target));
    command
}

fn namespace_create_command(
    namespace_name: &ployz_core::corrosion::CorrosionNamespaceName,
    target: Option<&SshTarget>,
) -> String {
    format!(
        "ployz namespace create {}{}",
        shell_quote(namespace_name.as_str()),
        target_suffix(target)
    )
}

fn target_suffix(target: Option<&SshTarget>) -> String {
    target.map_or_else(String::new, |target| {
        format!(" --target {}", shell_quote(target.as_str()))
    })
}

#[derive(Debug, thiserror::Error)]
pub enum RouteExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error(transparent)]
    Remove(#[from] crate::removal::RemovalExecutionError),
    #[error("{message}")]
    Refused { message: String },
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{CorrosionNamespaceName, CorrosionServiceName, IngressMode};
    use ployz_core::ids::{NamespaceRowId, RouteBindingRowId, ServiceRowId};
    use ployz_core::operation::{RouteHostname, RoutePort};
    use ployz_core::{RouteAttachRefusal, RouteAttachRequest, RouteRemoveRequest};

    use super::*;

    const ID_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const ID_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    fn request() -> RouteAttachRequest {
        RouteAttachRequest {
            hostname: RouteHostname::try_new("web.example.com").expect("hostname"),
            namespace_name: CorrosionNamespaceName::try_new("production").expect("namespace"),
            namespace_id: Some(NamespaceRowId::try_new(ID_A).expect("namespace id")),
            service_name: CorrosionServiceName::try_new("web").expect("service"),
            service_id: Some(ServiceRowId::try_new(ID_A).expect("service id")),
            endpoint_port: RoutePort::try_new(8080).expect("port"),
            ingress_mode: IngressMode::Direct,
        }
    }

    #[test]
    fn hostname_collision_names_the_exact_remove_primitive() {
        let hostname = RouteHostname::try_new("web.example.com").expect("hostname");
        let route_id = RouteBindingRowId::try_new(ID_A).expect("route id");

        assert_eq!(
            refusal_message(
                RouteAttachRefusal::HostnameAlreadyAttached {
                    hostname: hostname.clone(),
                    route_id: route_id.clone(),
                    remove: RouteRemoveRequest {
                        hostname,
                        route_id: Some(route_id),
                    },
                },
                &request(),
                None
            ),
            "hostname web.example.com is already attached by route 01ARZ3NDEKTSV4RRFFQ69G5FAV; remove it first with `ployz route rm 'web.example.com' --id '01ARZ3NDEKTSV4RRFFQ69G5FAV'`"
        );
    }

    #[test]
    fn copy_ready_attach_commands_quote_every_dynamic_token() {
        let request = request();
        let target = "root@-proxy".parse::<SshTarget>().expect("target");

        assert_eq!(
            attach_command(
                &request,
                request.namespace_id.as_ref(),
                request.service_id.as_ref(),
                Some(&target),
            ),
            "ployz route attach 'web.example.com' --namespace 'production' --service 'web' --port '8080' --namespace-id '01ARZ3NDEKTSV4RRFFQ69G5FAV' --service-id '01ARZ3NDEKTSV4RRFFQ69G5FAV' --target 'root@-proxy'"
        );
    }

    #[test]
    fn every_attach_refusal_names_a_copy_ready_resolving_command() {
        let namespace = CorrosionNamespaceName::try_new("production").expect("namespace");
        let other_namespace = CorrosionNamespaceName::try_new("staging").expect("namespace");
        let service = CorrosionServiceName::try_new("web").expect("service");
        let other_service = CorrosionServiceName::try_new("api").expect("service");
        let namespace_id = NamespaceRowId::try_new(ID_A).expect("namespace id");
        let other_namespace_id = NamespaceRowId::try_new(ID_B).expect("namespace id");
        let service_id = ServiceRowId::try_new(ID_A).expect("service id");
        let other_service_id = ServiceRowId::try_new(ID_B).expect("service id");
        let route_id = RouteBindingRowId::try_new(ID_A).expect("route id");
        let hostname = RouteHostname::try_new("web.example.com").expect("hostname");
        let refusals = vec![
            RouteAttachRefusal::UnsupportedIngressMode {
                requested: IngressMode::TailscaleFunnel,
            },
            RouteAttachRefusal::NamespaceNotFound {
                namespace_name: namespace.clone(),
            },
            RouteAttachRefusal::NamespaceAmbiguous {
                namespace_name: namespace.clone(),
                namespace_ids: vec![namespace_id.clone(), other_namespace_id.clone()],
            },
            RouteAttachRefusal::NamespaceIdMismatch {
                namespace_name: namespace.clone(),
                requested: other_namespace_id.clone(),
                found: namespace_id.clone(),
            },
            RouteAttachRefusal::NamespaceIdentityMismatch {
                namespace_id: namespace_id.clone(),
                requested_name: namespace.clone(),
                found_name: other_namespace,
            },
            RouteAttachRefusal::NamespaceStoredRowUnselectable {
                namespace_id: namespace_id.clone(),
            },
            RouteAttachRefusal::ServiceNotFound {
                namespace_id: namespace_id.clone(),
                service_name: service.clone(),
            },
            RouteAttachRefusal::ServiceAmbiguous {
                namespace_id: namespace_id.clone(),
                service_name: service.clone(),
                service_ids: vec![service_id.clone(), other_service_id.clone()],
            },
            RouteAttachRefusal::ServiceIdMismatch {
                namespace_id: namespace_id.clone(),
                service_name: service.clone(),
                requested: other_service_id.clone(),
                found: service_id.clone(),
            },
            RouteAttachRefusal::ServiceIdentityMismatch {
                service_id: service_id.clone(),
                requested_namespace_id: namespace_id,
                requested_name: service,
                found_namespace_id: other_namespace_id,
                found_name: other_service,
            },
            RouteAttachRefusal::ServiceStoredRowUnselectable { service_id },
            RouteAttachRefusal::HostnameAlreadyAttached {
                hostname: hostname.clone(),
                route_id: route_id.clone(),
                remove: RouteRemoveRequest {
                    hostname,
                    route_id: Some(route_id),
                },
            },
        ];

        for refusal in refusals {
            let message = refusal_message(refusal, &request(), None);
            assert!(message.contains("`ployz "), "missing command: {message}");
        }
    }
}
