use ployz_core::ids::ServiceId;
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

pub fn parse_service_list_command(args: &[String]) -> Result<ServiceListCommand, PloyzctlCliError> {
    match args {
        [] => Ok(ServiceListCommand),
        [unexpected, ..] => Err(PloyzctlCliError::UnexpectedArgument {
            value: unexpected.clone(),
        }),
    }
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
    let service_id = match args {
        [] => {
            return Err(PloyzctlCliError::MissingRequiredArgument {
                flag: "<service_id>",
            });
        }
        [service_id] => service_id,
        [_, unexpected, ..] => {
            return Err(PloyzctlCliError::UnexpectedArgument {
                value: unexpected.clone(),
            });
        }
    };
    let service_id = ServiceId::try_new(service_id.clone())
        .map_err(|error| invalid_value("<service_id>", error))?;

    Ok(ServiceInspectCommand { service_id })
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
