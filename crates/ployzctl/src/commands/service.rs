use clap::Args;
use ployz_core::ids::{NamespaceId, ServiceId};
use ployz_sdk_types::{
    ServiceInspectRequest, ServiceListRequest, ServiceListResult, ServiceSnapshot,
};

use crate::commands::{PloyzctlCliError, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceListCommand;

impl ServiceListCommand {
    #[must_use]
    pub const fn into_request(self) -> ServiceListRequest {
        ServiceListRequest {}
    }
}

pub(crate) fn service_list_command(_: EmptyCli) -> ServiceListCommand {
    ServiceListCommand
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInspectCommand {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
}

impl ServiceInspectCommand {
    #[must_use]
    pub fn into_request(self) -> ServiceInspectRequest {
        ServiceInspectRequest {
            namespace_id: self.namespace_id,
            service_id: self.service_id,
        }
    }
}

pub(crate) fn service_inspect_command(
    parsed: ServiceInspectCli,
) -> Result<ServiceInspectCommand, PloyzctlCliError> {
    let namespace_id = parsed
        .namespace
        .map(NamespaceId::try_new)
        .transpose()
        .map_err(|error| invalid_value("--namespace", error))?
        .unwrap_or_else(|| NamespaceId::try_new("default").expect("default namespace is valid"));
    let service_id = ServiceId::try_new(parsed.service_id)
        .map_err(|error| invalid_value("<service_id>", error))?;

    Ok(ServiceInspectCommand {
        namespace_id,
        service_id,
    })
}

#[derive(Debug, Args)]
pub(crate) struct EmptyCli {}

#[derive(Debug, Args)]
pub(crate) struct ServiceInspectCli {
    #[arg(short = 'n', long = "namespace")]
    namespace: Option<String>,
    service_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceListOutput {
    pub services: Vec<ServiceSnapshot>,
}

impl ServiceListOutput {
    #[must_use]
    pub fn from_result(result: ServiceListResult) -> Self {
        Self {
            services: result.services,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let rendered = self
            .services
            .iter()
            .map(render_service_summary)
            .collect::<Vec<_>>()
            .join("\n");

        if rendered.is_empty() {
            rendered
        } else {
            rendered + "\n"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInspectOutput {
    pub service: ServiceSnapshot,
}

impl ServiceInspectOutput {
    #[must_use]
    pub fn new(service: ServiceSnapshot) -> Self {
        Self { service }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "service {}\nactive-revision {}\n",
            self.service.active.service_id.as_str(),
            self.service.active.namespace_revision_entry_id.as_str(),
        )
    }
}

fn render_service_summary(service: &ServiceSnapshot) -> String {
    format!(
        "{} active-revision {}",
        service.active.service_id.as_str(),
        service.active.namespace_revision_entry_id.as_str(),
    )
}
