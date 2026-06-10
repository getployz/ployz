use clap::Parser;
use ployz_core::ids::ServiceId;
use ployz_sdk_types::{
    ServiceInspectRequest, ServiceListRequest, ServiceListResult, ServiceSnapshot,
};

use crate::commands::{PloyzctlCliError, clap_error, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceListCommand;

impl ServiceListCommand {
    #[must_use]
    pub const fn into_request(self) -> ServiceListRequest {
        ServiceListRequest {}
    }
}

pub fn parse_service_list_command(args: &[String]) -> Result<ServiceListCommand, PloyzctlCliError> {
    EmptyCli::try_parse_from(
        std::iter::once("service list".to_owned()).chain(args.iter().cloned()),
    )
    .map_err(clap_error)?;
    Ok(ServiceListCommand)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInspectCommand {
    pub service_id: ServiceId,
}

impl ServiceInspectCommand {
    #[must_use]
    pub fn into_request(self) -> ServiceInspectRequest {
        ServiceInspectRequest {
            service_id: self.service_id,
        }
    }
}

pub fn parse_service_inspect_command(
    args: &[String],
) -> Result<ServiceInspectCommand, PloyzctlCliError> {
    let parsed = ServiceInspectCli::try_parse_from(
        std::iter::once("service inspect".to_owned()).chain(args.iter().cloned()),
    )
    .map_err(clap_error)?;
    let service_id = ServiceId::try_new(parsed.service_id)
        .map_err(|error| invalid_value("<service_id>", error))?;

    Ok(ServiceInspectCommand { service_id })
}

#[derive(Debug, Parser)]
struct EmptyCli {}

#[derive(Debug, Parser)]
#[command(name = "service inspect")]
struct ServiceInspectCli {
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
            self.service.active.active_revision.as_str(),
        )
    }
}

fn render_service_summary(service: &ServiceSnapshot) -> String {
    format!(
        "{} active-revision {}",
        service.active.service_id.as_str(),
        service.active.active_revision.as_str(),
    )
}
