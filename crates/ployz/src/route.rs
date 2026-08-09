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
        service_name: command.service,
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
            outcome: RouteAttachOutcome::Attached,
        }) => Ok(format!("attached route {hostname}\n")),
        JsonReply::Success(RouteAttachReply {
            outcome: RouteAttachOutcome::AlreadyAttached,
        }) => Ok(format!("route {hostname} is already attached\n")),
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
            attach_command(request, target)
        ),
        RouteAttachRefusal::NamespaceNotFound { namespace_name } => format!(
            "namespace {} does not exist; create it with `{}`, then retry `{}`",
            namespace_name.as_str(),
            namespace_create_command(&namespace_name, target),
            attach_command(request, target)
        ),
        RouteAttachRefusal::NamespaceStoredRowUnselectable { namespace_name } => format!(
            "namespace {} exists but its row is invalid; inspect the namespaces lens before retrying `{}`",
            namespace_name.as_str(),
            attach_command(request, target)
        ),
        RouteAttachRefusal::ServiceNotFound {
            namespace_name,
            service_name,
        } => format!(
            "service {}/{} does not exist; add it to the complete namespace deploy manifest, then retry `{}`",
            namespace_name.as_str(),
            service_name.as_str(),
            attach_command(request, target)
        ),
        RouteAttachRefusal::ServiceStoredRowUnselectable {
            namespace_name,
            service_name,
        } => format!(
            "service {}/{} exists but its row is invalid; inspect the services lens before retrying `{}`",
            namespace_name.as_str(),
            service_name.as_str(),
            attach_command(request, target)
        ),
        RouteAttachRefusal::HostnameAlreadyAttached {
            hostname, remove, ..
        } => format!(
            "hostname {} is already attached; remove it first with `ployz route rm {}{}`",
            hostname.as_str(),
            shell_quote(remove.hostname.as_str()),
            target_suffix(target),
        ),
    }
}

fn attach_command(request: &RouteAttachRequest, target: Option<&SshTarget>) -> String {
    format!(
        "ployz route attach {} --namespace {} --service {} --port {}",
        shell_quote(request.hostname.as_str()),
        shell_quote(request.namespace_name.as_str()),
        shell_quote(request.service_name.as_str()),
        shell_quote(&request.endpoint_port.get().to_string()),
    ) + &target_suffix(target)
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
    use ployz_core::ids::RouteHostname;
    use ployz_core::operation::RoutePort;
    use ployz_core::{RouteAttachRefusal, RouteAttachRequest, RouteRemoveRequest};

    use super::*;

    fn request() -> RouteAttachRequest {
        RouteAttachRequest {
            hostname: RouteHostname::try_new("web.example.com").expect("hostname"),
            namespace_name: CorrosionNamespaceName::try_new("production").expect("namespace"),
            service_name: CorrosionServiceName::try_new("web").expect("service"),
            endpoint_port: RoutePort::try_new(8080).expect("port"),
            ingress_mode: IngressMode::Direct,
        }
    }

    #[test]
    fn hostname_collision_names_the_exact_remove_primitive() {
        let hostname = RouteHostname::try_new("web.example.com").expect("hostname");
        assert_eq!(
            refusal_message(
                RouteAttachRefusal::HostnameAlreadyAttached {
                    hostname: hostname.clone(),
                    remove: RouteRemoveRequest { hostname },
                },
                &request(),
                None
            ),
            "hostname web.example.com is already attached; remove it first with `ployz route rm 'web.example.com'`"
        );
    }

    #[test]
    fn copy_ready_attach_commands_quote_every_dynamic_token() {
        let request = request();
        let target = "root@-proxy".parse::<SshTarget>().expect("target");

        assert_eq!(
            attach_command(&request, Some(&target)),
            "ployz route attach 'web.example.com' --namespace 'production' --service 'web' --port '8080' --target 'root@-proxy'"
        );
    }

    #[test]
    fn every_attach_refusal_names_a_copy_ready_resolving_command() {
        let namespace = CorrosionNamespaceName::try_new("production").expect("namespace");
        let service = CorrosionServiceName::try_new("web").expect("service");
        let hostname = RouteHostname::try_new("web.example.com").expect("hostname");
        let refusals = vec![
            RouteAttachRefusal::UnsupportedIngressMode {
                requested: IngressMode::TailscaleFunnel,
            },
            RouteAttachRefusal::NamespaceNotFound {
                namespace_name: namespace.clone(),
            },
            RouteAttachRefusal::NamespaceStoredRowUnselectable {
                namespace_name: namespace.clone(),
            },
            RouteAttachRefusal::ServiceNotFound {
                namespace_name: namespace.clone(),
                service_name: service.clone(),
            },
            RouteAttachRefusal::ServiceStoredRowUnselectable {
                namespace_name: namespace,
                service_name: service,
            },
            RouteAttachRefusal::HostnameAlreadyAttached {
                hostname: hostname.clone(),
                remove: RouteRemoveRequest { hostname },
            },
        ];

        for refusal in refusals {
            let message = refusal_message(refusal, &request(), None);
            assert!(message.contains("`ployz "), "missing command: {message}");
        }
    }
}
