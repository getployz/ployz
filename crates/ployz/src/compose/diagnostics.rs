#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ComposePath(String);

impl ComposePath {
    #[must_use]
    pub(crate) fn root() -> Self {
        Self(String::new())
    }

    #[must_use]
    pub(crate) fn field(&self, field: &str) -> Self {
        if self.0.is_empty() {
            return Self(field.to_owned());
        }
        Self(format!("{}.{}", self.0, field))
    }

    #[must_use]
    pub(crate) fn index(&self, index: usize) -> Self {
        Self(format!("{}[{index}]", self.0))
    }

    #[must_use]
    pub(crate) fn render(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComposeFinding {
    pub path: ComposePath,
    pub kind: ComposeFindingKind,
}

impl ComposeFinding {
    #[must_use]
    pub(crate) fn invalid(path: ComposePath, message: impl std::fmt::Display) -> Self {
        Self {
            path,
            kind: ComposeFindingKind::InvalidValue {
                message: message.to_string(),
            },
        }
    }

    #[must_use]
    pub(crate) fn unsupported(path: ComposePath, feature: KnownUnsupported) -> Self {
        Self {
            path,
            kind: ComposeFindingKind::Unsupported { feature },
        }
    }

    #[must_use]
    pub(crate) fn unknown(path: ComposePath) -> Self {
        Self {
            path,
            kind: ComposeFindingKind::UnknownField,
        }
    }

    #[must_use]
    pub(crate) fn advisory(path: ComposePath, message: impl Into<String>) -> Self {
        Self {
            path,
            kind: ComposeFindingKind::Advisory {
                message: message.into(),
            },
        }
    }

    fn is_fatal(&self, mode: UnsupportedFieldMode) -> bool {
        match (&self.kind, mode) {
            (ComposeFindingKind::InvalidValue { .. }, UnsupportedFieldMode::Strict)
            | (ComposeFindingKind::InvalidValue { .. }, UnsupportedFieldMode::AllowUnsupported)
            | (ComposeFindingKind::Unsupported { .. }, UnsupportedFieldMode::Strict)
            | (ComposeFindingKind::UnknownField, UnsupportedFieldMode::Strict) => true,
            (ComposeFindingKind::Unsupported { .. }, UnsupportedFieldMode::AllowUnsupported)
            | (ComposeFindingKind::UnknownField, UnsupportedFieldMode::AllowUnsupported)
            | (ComposeFindingKind::Advisory { .. }, UnsupportedFieldMode::Strict)
            | (ComposeFindingKind::Advisory { .. }, UnsupportedFieldMode::AllowUnsupported) => {
                false
            }
        }
    }

    fn render(&self) -> RenderedWarning {
        RenderedWarning(format!("{}  {}", self.path.render(), self.render_detail()))
    }

    fn render_warning(&self) -> RenderedWarning {
        RenderedWarning(format!(
            "{}  warning  {}",
            self.path.render(),
            self.render_detail()
        ))
    }

    fn render_detail(&self) -> String {
        match &self.kind {
            ComposeFindingKind::InvalidValue { message } => {
                format!("invalid value  {message}")
            }
            ComposeFindingKind::Unsupported { feature } => {
                format!(
                    "unsupported ({})  {}; remove it or pass --allow-unsupported",
                    feature.status(),
                    feature.guidance()
                )
            }
            ComposeFindingKind::UnknownField => {
                "unknown field  remove it or pass --allow-unsupported".to_owned()
            }
            ComposeFindingKind::Advisory { message } => {
                format!("advisory  {message}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposeFindingKind {
    InvalidValue { message: String },
    Unsupported { feature: KnownUnsupported },
    UnknownField,
    Advisory { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedFieldMode {
    Strict,
    AllowUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KnownUnsupported {
    Build,
    CgroupParent,
    Configs,
    DeployMode,
    DeployPlacement,
    DeployResources,
    DeployRestartPolicy,
    DeployUpdateConfig,
    Devices,
    Dns,
    DnsSearch,
    Expose,
    ExtraHosts,
    Init,
    Labels,
    Logging,
    Networks,
    Platform,
    Ports,
    Privileged,
    Profiles,
    PullPolicy,
    Secrets,
    SecurityOpt,
    TopLevelNetworks,
    TopLevelVolumes,
    Ulimits,
    User,
    Volumes,
    WorkingDir,
}

impl KnownUnsupported {
    #[must_use]
    pub(crate) const fn status(self) -> &'static str {
        match self {
            Self::Build
            | Self::Configs
            | Self::DeployMode
            | Self::DeployPlacement
            | Self::DeployResources
            | Self::DeployUpdateConfig
            | Self::DeployRestartPolicy
            | Self::Networks
            | Self::Ports
            | Self::Profiles
            | Self::Secrets
            | Self::TopLevelNetworks
            | Self::TopLevelVolumes
            | Self::Volumes => "planned",
            Self::CgroupParent
            | Self::Devices
            | Self::Dns
            | Self::DnsSearch
            | Self::Expose
            | Self::ExtraHosts
            | Self::Init
            | Self::Labels
            | Self::Logging
            | Self::Platform
            | Self::Privileged
            | Self::PullPolicy
            | Self::SecurityOpt
            | Self::Ulimits
            | Self::User
            | Self::WorkingDir => "unsupported",
        }
    }

    #[must_use]
    pub(crate) const fn guidance(self) -> &'static str {
        match self {
            Self::Build => "build images before deploy",
            Self::Devices | Self::Privileged | Self::SecurityOpt => {
                "host capability controls are not deployed yet"
            }
            Self::CgroupParent => "cgroup parent is not part of the deploy model",
            Self::Configs => "configs are not deployed yet",
            Self::DeployMode => "global deploy mode is not deployed yet",
            Self::DeployPlacement => "placement constraints are not deployed yet",
            Self::DeployResources => "resource reservations are not deployed yet",
            Self::DeployRestartPolicy => "restart policy subfields are not deployed yet",
            Self::DeployUpdateConfig => "update order is not deployed yet",
            Self::Dns | Self::DnsSearch => "custom DNS settings are not deployed yet",
            Self::Expose | Self::Ports => "use x-ports for Ployz Route Bindings",
            Self::ExtraHosts => "extra hosts are not deployed yet",
            Self::Init => "init process selection is not deployed yet",
            Self::Labels => "container labels are owned by Ployz",
            Self::Logging => "logging driver settings are not deployed yet",
            Self::Networks => "custom networks are not deployed yet",
            Self::Platform => "platform selection is not deployed yet",
            Self::Profiles => "profile resolution is deferred",
            Self::PullPolicy => "pull policy is not deployed yet",
            Self::Secrets => "secrets are planned separately",
            Self::TopLevelNetworks => "top-level networks are not deployed yet",
            Self::TopLevelVolumes => "top-level named volumes are not deployed yet",
            Self::Ulimits => "ulimits are not deployed yet",
            Self::User => "container user is not deployed yet",
            Self::Volumes => "volumes are not deployed yet",
            Self::WorkingDir => "working directory is not deployed yet",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedWarning(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComposeRejection {
    pub rendered: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ComposeDiagnostics {
    findings: Vec<ComposeFinding>,
}

impl ComposeDiagnostics {
    #[must_use]
    pub(crate) fn new(findings: Vec<ComposeFinding>) -> Self {
        Self { findings }
    }

    pub(crate) fn resolve(
        &self,
        mode: UnsupportedFieldMode,
    ) -> Result<Vec<RenderedWarning>, ComposeRejection> {
        let mut findings = self.findings.clone();
        findings.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.render().0.cmp(&right.render().0))
        });
        if findings.iter().any(|finding| finding.is_fatal(mode)) {
            let rendered = findings
                .iter()
                .map(ComposeFinding::render)
                .collect::<Vec<_>>();
            return Err(ComposeRejection {
                rendered: rendered
                    .into_iter()
                    .map(|line| line.0)
                    .collect::<Vec<_>>()
                    .join("\n"),
            });
        }
        Ok(findings
            .iter()
            .map(ComposeFinding::render_warning)
            .collect())
    }
}

#[must_use]
pub(crate) fn classify_service_key(key: &str) -> Option<KnownUnsupported> {
    match key {
        "build" => Some(KnownUnsupported::Build),
        "cgroup_parent" => Some(KnownUnsupported::CgroupParent),
        "configs" => Some(KnownUnsupported::Configs),
        "deploy.mode" => Some(KnownUnsupported::DeployMode),
        "deploy.placement" => Some(KnownUnsupported::DeployPlacement),
        "deploy.resources" => Some(KnownUnsupported::DeployResources),
        "deploy.restart_policy" => Some(KnownUnsupported::DeployRestartPolicy),
        "deploy.update_config" => Some(KnownUnsupported::DeployUpdateConfig),
        "devices" => Some(KnownUnsupported::Devices),
        "dns" => Some(KnownUnsupported::Dns),
        "dns_search" => Some(KnownUnsupported::DnsSearch),
        "expose" => Some(KnownUnsupported::Expose),
        "extra_hosts" => Some(KnownUnsupported::ExtraHosts),
        "init" => Some(KnownUnsupported::Init),
        "labels" => Some(KnownUnsupported::Labels),
        "logging" => Some(KnownUnsupported::Logging),
        "networks" => Some(KnownUnsupported::Networks),
        "platform" => Some(KnownUnsupported::Platform),
        "ports" => Some(KnownUnsupported::Ports),
        "privileged" => Some(KnownUnsupported::Privileged),
        "profiles" => Some(KnownUnsupported::Profiles),
        "pull_policy" => Some(KnownUnsupported::PullPolicy),
        "secrets" => Some(KnownUnsupported::Secrets),
        "security_opt" => Some(KnownUnsupported::SecurityOpt),
        "ulimits" => Some(KnownUnsupported::Ulimits),
        "user" => Some(KnownUnsupported::User),
        "volumes" => Some(KnownUnsupported::Volumes),
        "working_dir" => Some(KnownUnsupported::WorkingDir),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ComposeDiagnostics, ComposeFinding, ComposePath, KnownUnsupported, UnsupportedFieldMode,
    };

    #[test]
    fn strict_rejects_unsupported_and_unknown_but_allow_warns() {
        let diagnostics = ComposeDiagnostics::new(vec![
            ComposeFinding::unsupported(
                ComposePath::root()
                    .field("services")
                    .field("web")
                    .field("ports"),
                KnownUnsupported::Ports,
            ),
            ComposeFinding::unknown(
                ComposePath::root()
                    .field("services")
                    .field("web")
                    .field("mystery"),
            ),
        ]);

        assert!(
            diagnostics
                .resolve(UnsupportedFieldMode::Strict)
                .expect_err("strict rejects")
                .rendered
                .contains("services.web.ports")
        );
        assert_eq!(
            diagnostics
                .resolve(UnsupportedFieldMode::AllowUnsupported)
                .expect("allow warns")
                .len(),
            2
        );
    }

    #[test]
    fn invalid_values_are_fatal_in_allow_mode() {
        let diagnostics = ComposeDiagnostics::new(vec![ComposeFinding::invalid(
            ComposePath::root()
                .field("services")
                .field("web")
                .field("image"),
            "missing image",
        )]);

        assert!(
            diagnostics
                .resolve(UnsupportedFieldMode::AllowUnsupported)
                .is_err()
        );
    }
}
