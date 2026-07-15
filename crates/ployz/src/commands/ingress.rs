use clap::{Args, ValueEnum};
use ployz_core::ids::OperationId;
use ployz_core::ingress::{
    AutomaticHostnameConfiguration, AutomaticHostnameSuffix, IngressConfiguration,
    PloyzDnsTargetIntent,
};
use ployz_sdk_types::IngressConfigureRequest;

use crate::client_ids::generate_client_ingress_configure_id;
use crate::commands::{PloyzctlCliError, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressConfigureCommand {
    pub operation_id: OperationId,
    pub configuration: IngressConfiguration,
}

impl IngressConfigureCommand {
    #[must_use]
    pub fn into_request(self) -> IngressConfigureRequest {
        IngressConfigureRequest {
            operation_id: self.operation_id,
            configuration: self.configuration,
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct IngressConfigureCli {
    #[arg(long)]
    dns_target: DnsTargetCli,
    #[arg(long, value_name = "disabled|ployz|custom:<suffix>")]
    automatic_hostnames: AutomaticHostnamesCli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum DnsTargetCli {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutomaticHostnamesCli {
    Disabled,
    Ployz,
    Custom { suffix: AutomaticHostnameSuffix },
}

impl std::str::FromStr for AutomaticHostnamesCli {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "disabled" => Ok(Self::Disabled),
            "ployz" => Ok(Self::Ployz),
            value => {
                let Some(suffix) = value.strip_prefix("custom:") else {
                    return Err("expected disabled, ployz, or custom:<suffix>".to_owned());
                };
                Ok(Self::Custom {
                    suffix: AutomaticHostnameSuffix::try_new(suffix)
                        .map_err(|error| error.to_string())?,
                })
            }
        }
    }
}

impl DnsTargetCli {
    pub(crate) const fn intent(self) -> PloyzDnsTargetIntent {
        match self {
            Self::Enabled => PloyzDnsTargetIntent::Enabled,
            Self::Disabled => PloyzDnsTargetIntent::Disabled,
        }
    }
}

impl AutomaticHostnamesCli {
    pub(crate) fn configuration(self) -> AutomaticHostnameConfiguration {
        match self {
            Self::Disabled => AutomaticHostnameConfiguration::Disabled,
            Self::Ployz => AutomaticHostnameConfiguration::Ployz,
            Self::Custom { suffix } => AutomaticHostnameConfiguration::Custom { suffix },
        }
    }
}

pub(crate) fn ingress_configure_command(
    parsed: IngressConfigureCli,
) -> Result<IngressConfigureCommand, PloyzctlCliError> {
    let ployz_dns_target = parsed.dns_target.intent();
    let automatic_hostnames = parsed.automatic_hostnames.configuration();
    let configuration = IngressConfiguration::try_new(automatic_hostnames, ployz_dns_target)
        .map_err(|error| invalid_value("ingress configuration", error))?;
    Ok(IngressConfigureCommand {
        operation_id: generate_client_ingress_configure_id()
            .map_err(|error| invalid_value("ingress configure", error))?
            .operation_id,
        configuration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_custom_automatic_hostname_configuration() {
        let command = ingress_configure_command(IngressConfigureCli {
            dns_target: DnsTargetCli::Disabled,
            automatic_hostnames: "custom:Apps.Example.COM."
                .parse()
                .expect("valid custom suffix"),
        })
        .expect("valid configuration");

        assert!(matches!(
            command.configuration.automatic_hostnames(),
            AutomaticHostnameConfiguration::Custom { suffix }
                if suffix.as_str() == "apps.example.com"
        ));
    }

    #[test]
    fn cli_accepts_the_ingress_configure_spelling() {
        let command = crate::commands::parse_command(
            [
                "ingress",
                "configure",
                "--dns-target",
                "enabled",
                "--automatic-hostnames",
                "ployz",
            ]
            .map(str::to_owned),
        )
        .expect("parse ingress configure");

        let crate::commands::PloyzctlCommand::IngressConfigure(command) = command else {
            panic!("expected ingress configure command")
        };
        assert_eq!(
            command.configuration,
            IngressConfiguration::try_new(
                AutomaticHostnameConfiguration::Ployz,
                PloyzDnsTargetIntent::Enabled,
            )
            .expect("valid ingress configuration")
        );
    }

    #[test]
    fn rejects_ployz_hostnames_without_dns_target() {
        let result = ingress_configure_command(IngressConfigureCli {
            dns_target: DnsTargetCli::Disabled,
            automatic_hostnames: AutomaticHostnamesCli::Ployz,
        });

        assert!(result.is_err());
    }
}
