use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::model::{DeployPhaseAdvancePolicy, DeployPhaseCommitPolicy, DeployPhaseRollbackPolicy};

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct Namespace(pub String);

impl AsRef<str> for Namespace {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Namespace {
    #[must_use]
    pub fn system() -> Self {
        Self("system".into())
    }

    #[must_use]
    pub fn default_ns() -> Self {
        Self("default".into())
    }

    #[must_use]
    pub fn is_system(&self) -> bool {
        self.0 == "system"
    }
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self(inner) = self;
        f.write_str(inner)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeployManifest {
    pub namespace: Namespace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<DeployIntent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<VolumeDeclaration>,
    pub services: Vec<ServiceSpec>,
}

impl DeployManifest {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_namespace_name(&self.namespace.0) {
            return Err(format!(
                "manifest namespace '{}' must be 1-63 chars of [a-z0-9_-], starting with a letter or digit",
                self.namespace
            ));
        }

        let mut volumes_by_name = BTreeMap::new();
        for volume in &self.volumes {
            volume.validate()?;
            if volumes_by_name
                .insert(volume.name.clone(), volume)
                .is_some()
            {
                return Err(format!(
                    "manifest contains duplicate volume '{}'",
                    volume.name
                ));
            }
        }

        let mut seen = BTreeSet::new();
        for service in &self.services {
            if !seen.insert(service.name.clone()) {
                return Err(format!(
                    "manifest contains duplicate service '{}'",
                    service.name
                ));
            }
            service.validate()?;

            for mount in &service.template.mounts {
                match &mount.source {
                    MountSource::Volume(name) => {
                        let Some(volume) = volumes_by_name.get(name) else {
                            return Err(format!(
                                "service '{}' references undeclared volume '{}'",
                                service.name, name
                            ));
                        };
                        if volume.scope == VolumeScope::Single
                            && matches!(service.placement, Placement::Replicated { count } if count > 1)
                        {
                            return Err(format!(
                                "service '{}' has replicas > 1 but mounts volume '{}' with scope=single",
                                service.name, name
                            ));
                        }
                    }
                    MountSource::Bind(_) if !self.namespace.is_system() => {
                        return Err(format!(
                            "service '{}' uses a bind mount, which is only allowed in the system namespace",
                            service.name
                        ));
                    }
                    MountSource::Bind(_) | MountSource::Tmpfs => {}
                }
            }
        }

        if let Some(intent) = &self.intent {
            intent.validate(self)?;
            intent.validate_planning_support()?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeployIntent {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceIntentHint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<VolumeIntentHint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<DeployPhaseIntent>,
}

impl DeployIntent {
    fn validate(&self, manifest: &DeployManifest) -> Result<(), String> {
        let service_names = manifest
            .services
            .iter()
            .map(|service| service.name.as_str())
            .collect::<BTreeSet<_>>();
        let volume_names = manifest
            .volumes
            .iter()
            .map(|volume| volume.name.as_str())
            .collect::<BTreeSet<_>>();

        let mut seen_services = BTreeSet::new();
        for hint in &self.services {
            hint.validate(&service_names)?;
            if !seen_services.insert(hint.service.as_str()) {
                return Err(format!(
                    "deploy intent contains duplicate service hint '{}'",
                    hint.service
                ));
            }
        }

        let mut seen_volumes = BTreeSet::new();
        for hint in &self.volumes {
            hint.validate(&volume_names)?;
            if !seen_volumes.insert(hint.volume.as_str()) {
                return Err(format!(
                    "deploy intent contains duplicate volume hint '{}'",
                    hint.volume
                ));
            }
        }

        let mut seen_phases = BTreeSet::new();
        let mut phased_services = BTreeSet::new();
        let mut phased_volumes = BTreeSet::new();
        for phase in &self.phases {
            phase.validate(&service_names, &volume_names)?;
            if !seen_phases.insert(phase.phase_id.as_str()) {
                return Err(format!(
                    "deploy intent contains duplicate phase '{}'",
                    phase.phase_id
                ));
            }
            for after in &phase.after {
                if after == &phase.phase_id {
                    return Err(format!(
                        "deploy phase '{}' cannot depend on itself",
                        phase.phase_id
                    ));
                }
            }
            for service in &phase.services {
                if !phased_services.insert(service.as_str()) {
                    return Err(format!(
                        "deploy intent assigns service '{service}' to multiple phases"
                    ));
                }
            }
            for volume in &phase.volumes {
                if !phased_volumes.insert(volume.as_str()) {
                    return Err(format!(
                        "deploy intent assigns volume '{volume}' to multiple phases"
                    ));
                }
            }
        }
        validate_phase_dependencies(&self.phases)?;

        if self.services.is_empty() && self.volumes.is_empty() && self.phases.is_empty() {
            return Err(
                "deploy intent must include at least one service, volume, or phase hint".into(),
            );
        }

        Ok(())
    }

    fn validate_planning_support(&self) -> Result<(), String> {
        for hint in &self.services {
            match &hint.intent {
                ServiceIntent::Branch {
                    source_namespace: _,
                    source_service: _,
                } => {}
                ServiceIntent::Move { to_machine: _ } => {
                    return Err(format!(
                        "deploy intent move for service '{}' is not supported by deploy planning yet",
                        hint.service
                    ));
                }
                ServiceIntent::Portal {
                    source_namespace: _,
                    source_service: _,
                } => {
                    return Err(format!(
                        "deploy intent portal for service '{}' is not supported by deploy planning yet",
                        hint.service
                    ));
                }
            }
        }

        for hint in &self.volumes {
            match &hint.intent {
                VolumeIntent::Move {
                    from_machine: _,
                    to_machine: _,
                } => {}
            }
        }

        for phase in &self.phases {
            if phase.advance_policy != DeployPhaseAdvancePolicy::Immediate {
                return Err(format!(
                    "deploy phase '{}' advance policy {:?} is not supported by deploy planning yet",
                    phase.phase_id, phase.advance_policy
                ));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeployPhaseIntent {
    pub phase_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    #[serde(default = "default_phase_commit_policy")]
    pub commit_policy: DeployPhaseCommitPolicy,
    #[serde(default = "default_phase_rollback_policy")]
    pub rollback_policy: DeployPhaseRollbackPolicy,
    #[serde(default = "default_phase_advance_policy")]
    pub advance_policy: DeployPhaseAdvancePolicy,
}

impl DeployPhaseIntent {
    fn validate(
        &self,
        service_names: &BTreeSet<&str>,
        volume_names: &BTreeSet<&str>,
    ) -> Result<(), String> {
        if !valid_storage_segment(&self.phase_id) {
            return Err(format!(
                "deploy phase id '{}' must be 1-63 chars of [a-z0-9_-], starting with a letter or digit",
                self.phase_id
            ));
        }
        if self.services.is_empty()
            && self.volumes.is_empty()
            && self.commit_policy != DeployPhaseCommitPolicy::NoStoreCommit
        {
            return Err(format!(
                "deploy phase '{}' must reference at least one service or volume",
                self.phase_id
            ));
        }
        if self.commit_policy == DeployPhaseCommitPolicy::NoStoreCommit
            && (!self.services.is_empty() || !self.volumes.is_empty())
        {
            return Err(format!(
                "deploy phase '{}' with no-store commit policy cannot reference services or volumes",
                self.phase_id
            ));
        }
        let mut seen_services = BTreeSet::new();
        for service in &self.services {
            if service.trim().is_empty() {
                return Err(format!(
                    "deploy phase '{}' cannot reference an empty service",
                    self.phase_id
                ));
            }
            if !service_names.contains(service.as_str()) {
                return Err(format!(
                    "deploy phase '{}' references unknown service '{}'",
                    self.phase_id, service
                ));
            }
            if !seen_services.insert(service.as_str()) {
                return Err(format!(
                    "deploy phase '{}' references service '{}' more than once",
                    self.phase_id, service
                ));
            }
        }
        let mut seen_volumes = BTreeSet::new();
        for volume in &self.volumes {
            if volume.trim().is_empty() {
                return Err(format!(
                    "deploy phase '{}' cannot reference an empty volume",
                    self.phase_id
                ));
            }
            if !volume_names.contains(volume.as_str()) {
                return Err(format!(
                    "deploy phase '{}' references unknown volume '{}'",
                    self.phase_id, volume
                ));
            }
            if !seen_volumes.insert(volume.as_str()) {
                return Err(format!(
                    "deploy phase '{}' references volume '{}' more than once",
                    self.phase_id, volume
                ));
            }
        }
        if matches!(self.commit_policy, DeployPhaseCommitPolicy::Checkpoint)
            && self.rollback_policy == DeployPhaseRollbackPolicy::Reversible
        {
            return Err(format!(
                "deploy phase '{}' with checkpoint commit policy must use a non-reversible rollback policy",
                self.phase_id
            ));
        }
        if matches!(self.commit_policy, DeployPhaseCommitPolicy::NoStoreCommit)
            && self.rollback_policy == DeployPhaseRollbackPolicy::Reversible
        {
            return Err(format!(
                "deploy phase '{}' with no-store commit policy must use a non-reversible rollback policy",
                self.phase_id
            ));
        }
        Ok(())
    }
}

fn default_phase_commit_policy() -> DeployPhaseCommitPolicy {
    DeployPhaseCommitPolicy::EndOfDeploy
}

fn default_phase_rollback_policy() -> DeployPhaseRollbackPolicy {
    DeployPhaseRollbackPolicy::Reversible
}

fn default_phase_advance_policy() -> DeployPhaseAdvancePolicy {
    DeployPhaseAdvancePolicy::Immediate
}

fn validate_phase_dependencies(phases: &[DeployPhaseIntent]) -> Result<(), String> {
    let phase_ids = phases
        .iter()
        .map(|phase| phase.phase_id.as_str())
        .collect::<BTreeSet<_>>();
    for phase in phases {
        for after in &phase.after {
            if !phase_ids.contains(after.as_str()) {
                return Err(format!(
                    "deploy phase '{}' depends on unknown phase '{}'",
                    phase.phase_id, after
                ));
            }
        }
    }

    for phase in phases {
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        visit_phase_dependency(phase.phase_id.as_str(), phases, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_phase_dependency<'a>(
    phase_id: &'a str,
    phases: &'a [DeployPhaseIntent],
    visiting: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
) -> Result<(), String> {
    if visited.contains(phase_id) {
        return Ok(());
    }
    if !visiting.insert(phase_id) {
        return Err(format!(
            "deploy phase dependencies contain a cycle involving '{phase_id}'"
        ));
    }
    let Some(phase) = phases
        .iter()
        .find(|candidate| candidate.phase_id == phase_id)
    else {
        return Ok(());
    };
    for after in &phase.after {
        visit_phase_dependency(after, phases, visiting, visited)?;
    }
    visiting.remove(phase_id);
    visited.insert(phase_id);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceIntentHint {
    pub service: String,
    pub intent: ServiceIntent,
}

impl ServiceIntentHint {
    fn validate(&self, service_names: &BTreeSet<&str>) -> Result<(), String> {
        if self.service.trim().is_empty() {
            return Err("deploy intent service hint cannot have an empty service".into());
        }
        if !service_names.contains(self.service.as_str()) {
            return Err(format!(
                "deploy intent references unknown service '{}'",
                self.service
            ));
        }
        self.intent.validate(&self.service)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceIntent {
    Branch {
        source_namespace: Namespace,
        source_service: String,
    },
    Move {
        to_machine: String,
    },
    Portal {
        source_namespace: Namespace,
        source_service: String,
    },
}

impl ServiceIntent {
    fn validate(&self, service: &str) -> Result<(), String> {
        match self {
            Self::Branch {
                source_namespace,
                source_service,
            } => validate_service_source("branch", service, source_namespace, source_service),
            Self::Move { to_machine } => validate_machine_hint("move", service, to_machine),
            Self::Portal {
                source_namespace,
                source_service,
            } => validate_service_source("portal", service, source_namespace, source_service),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeIntentHint {
    pub volume: String,
    pub intent: VolumeIntent,
}

impl VolumeIntentHint {
    fn validate(&self, volume_names: &BTreeSet<&str>) -> Result<(), String> {
        if self.volume.trim().is_empty() {
            return Err("deploy intent volume hint cannot have an empty volume".into());
        }
        if !volume_names.contains(self.volume.as_str()) {
            return Err(format!(
                "deploy intent references unknown volume '{}'",
                self.volume
            ));
        }
        self.intent.validate(&self.volume)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeIntent {
    Move {
        from_machine: String,
        to_machine: String,
    },
}

impl VolumeIntent {
    fn validate(&self, volume: &str) -> Result<(), String> {
        match self {
            Self::Move {
                from_machine,
                to_machine,
            } => {
                if from_machine.trim().is_empty() {
                    return Err(format!(
                        "deploy intent move for volume '{volume}' cannot have an empty from_machine"
                    ));
                }
                if to_machine.trim().is_empty() {
                    return Err(format!(
                        "deploy intent move for volume '{volume}' cannot have an empty to_machine"
                    ));
                }
                Ok(())
            }
        }
    }
}

fn validate_service_source(
    intent: &str,
    service: &str,
    source_namespace: &Namespace,
    source_service: &str,
) -> Result<(), String> {
    if !valid_namespace_name(&source_namespace.0) {
        return Err(format!(
            "deploy intent {intent} for service '{service}' has invalid source namespace '{}'",
            source_namespace
        ));
    }
    if source_service.trim().is_empty() {
        return Err(format!(
            "deploy intent {intent} for service '{service}' cannot have an empty source_service"
        ));
    }
    Ok(())
}

fn validate_machine_hint(intent: &str, service: &str, machine: &str) -> Result<(), String> {
    if machine.trim().is_empty() {
        return Err(format!(
            "deploy intent {intent} for service '{service}' cannot have an empty to_machine"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceSpec {
    // This type is serialized into ServiceRevisionRecord.spec_json and read by ployzd,
    // ployz-gateway, and ployz-dns. Shape changes must remain backward compatible during
    // rolling upgrades, or routing readers can reject new revisions and keep stale snapshots.
    pub name: String,
    pub placement: Placement,
    pub template: ContainerSpec,
    pub network: NetworkMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_ports: Vec<ServicePort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish: Vec<PublishedPort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<RouteSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<ReadinessProbe>,
    #[serde(default = "RolloutStrategy::recreate")]
    pub rollout: RolloutStrategy,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default = "RestartPolicy::unless_stopped")]
    pub restart: RestartPolicy,
}

impl ServiceSpec {
    #[must_use]
    pub fn fqn(&self, namespace: &Namespace) -> String {
        format!("{namespace}/{}", self.name)
    }

    pub fn canonical_revision_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| format!("serialize revision: {e}"))
    }

    pub fn revision_hash(&self) -> Result<String, String> {
        let json = self.canonical_revision_json()?;
        Ok(stable_hash_hex(json.as_bytes()))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("service name cannot be empty".into());
        }

        if let Some(pid_mode) = &self.template.pid_mode
            && pid_mode.trim().is_empty()
        {
            return Err(format!(
                "service '{}' template pid_mode cannot be empty",
                self.name
            ));
        }

        match self.placement {
            Placement::Global => {
                if self
                    .template
                    .mounts
                    .iter()
                    .any(|mount| matches!(mount.source, MountSource::Volume(_)))
                {
                    return Err(format!(
                        "service '{}' cannot use global placement with managed volumes",
                        self.name
                    ));
                }
            }
            Placement::Replicated { count } => {
                if count == 0 {
                    return Err(format!(
                        "service '{}' must request at least one replica",
                        self.name
                    ));
                }
            }
        }

        let mut seen_ports = BTreeSet::new();
        for port in &self.service_ports {
            if port.name.trim().is_empty() {
                return Err(format!(
                    "service '{}' has a service port with an empty name",
                    self.name
                ));
            }
            if !seen_ports.insert(port.name.clone()) {
                return Err(format!(
                    "service '{}' defines duplicate service port '{}'",
                    self.name, port.name
                ));
            }
        }

        for publish in &self.publish {
            if !seen_ports.contains(&publish.service_port) {
                return Err(format!(
                    "service '{}' publishes unknown service port '{}'",
                    self.name, publish.service_port
                ));
            }
        }

        for route in &self.routes {
            let (kind, service_port) = match route {
                RouteSpec::Http(r) => {
                    for hostname in &r.hostnames {
                        if hostname.trim().starts_with("*.") {
                            return Err(
                                "wildcard domains require DNS-01 managed public DNS, which is not enabled yet"
                                    .into(),
                            );
                        }
                    }
                    ("HTTP", &r.service_port)
                }
                RouteSpec::Tcp(r) => ("TCP", &r.service_port),
            };
            if !seen_ports.contains(service_port) {
                return Err(format!(
                    "service '{}' {kind} route references unknown service port '{service_port}'",
                    self.name
                ));
            }
        }

        if let Some(readiness) = &self.readiness {
            match &readiness.check {
                ReadinessCheck::Http { service_port, .. }
                | ReadinessCheck::Tcp { service_port } => {
                    if !seen_ports.contains(service_port) {
                        return Err(format!(
                            "service '{}' readiness probe references unknown service port '{}'",
                            self.name, service_port
                        ));
                    }
                }
                ReadinessCheck::Exec { command } => {
                    if command.is_empty() {
                        return Err(format!(
                            "service '{}' exec readiness probe must define at least one argument",
                            self.name
                        ));
                    }
                }
            }
            readiness.validate(&self.name)?;
        }

        match self.rollout {
            RolloutStrategy::Recreate => {}
            RolloutStrategy::BlueGreen => {
                if self.readiness.is_none() {
                    return Err(format!(
                        "service '{}' uses blue_green rollout but does not define readiness",
                        self.name
                    ));
                }
                if !self.publish.is_empty() {
                    return Err(format!(
                        "service '{}' cannot use blue_green rollout with published host ports",
                        self.name
                    ));
                }
                if matches!(self.network, NetworkMode::Host) {
                    return Err(format!(
                        "service '{}' cannot use blue_green rollout with host networking",
                        self.name
                    ));
                }
                if self
                    .template
                    .mounts
                    .iter()
                    .any(|mount| matches!(mount.source, MountSource::Bind(_)))
                {
                    return Err(format!(
                        "service '{}' cannot use blue_green rollout with bind mounts",
                        self.name
                    ));
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    Global,
    Replicated { count: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Overlay,
    #[serde(rename = "service")]
    Service(String),
    Host,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContainerSpec {
    pub image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, alias = "volumes", skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_add: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_drop: Vec<String>,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_grace_period: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_mode: Option<String>,
    #[serde(default = "PullPolicy::if_not_present")]
    pub pull_policy: PullPolicy,
    #[serde(default = "Resources::empty")]
    pub resources: Resources,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sysctls: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MountSource {
    Volume(String),
    Bind(String),
    Tmpfs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Mount {
    pub source: MountSource,
    pub target: String,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VolumeScope {
    Single,
    Shared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeDeclaration {
    pub name: String,
    pub scope: VolumeScope,
    pub quota: String,
    pub mode: String,
    pub owner: String,
}

impl VolumeDeclaration {
    fn validate(&self) -> Result<(), String> {
        if !valid_volume_name(&self.name) {
            return Err(format!(
                "volume name '{}' must be 1-63 chars of [a-z0-9_-], starting with a letter or digit",
                self.name
            ));
        }
        if !valid_quota(&self.quota) {
            return Err(format!(
                "volume '{}' quota must be an integer with optional K, M, G, or T suffix",
                self.name
            ));
        }
        if !valid_mode(&self.mode) {
            return Err(format!(
                "volume '{}' mode must be octal like 0750",
                self.name
            ));
        }
        if !valid_owner(&self.owner) {
            return Err(format!(
                "volume '{}' owner must be numeric uid:gid",
                self.name
            ));
        }
        Ok(())
    }
}

fn valid_volume_name(value: &str) -> bool {
    valid_storage_segment(value)
}

fn valid_namespace_name(value: &str) -> bool {
    valid_storage_segment(value)
}

fn valid_storage_segment(value: &str) -> bool {
    if value.is_empty() || value.len() > 63 {
        return false;
    }
    let Some(first) = value.chars().next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
}

fn valid_quota(value: &str) -> bool {
    parse_quota_bytes(value).is_ok()
}

/// Parse a ZFS-style quota string (`"20G"`, `"512M"`, `"1024"`) into bytes.
///
/// Suffixes use base-2 multipliers (K=1024, M=1024², G=1024³, T=1024⁴), matching
/// ZFS conventions. A bare integer is interpreted as bytes.
pub fn parse_quota_bytes(value: &str) -> Result<u64, String> {
    let Some(last) = value.chars().last() else {
        return Err("quota cannot be empty".into());
    };
    let (digits, multiplier) = match last {
        'K' => (&value[..value.len() - 1], 1024_u64),
        'M' => (&value[..value.len() - 1], 1024_u64.pow(2)),
        'G' => (&value[..value.len() - 1], 1024_u64.pow(3)),
        'T' => (&value[..value.len() - 1], 1024_u64.pow(4)),
        '0'..='9' => (value, 1),
        _ => return Err(format!("unsupported quota suffix in '{value}'")),
    };
    if digits.is_empty() {
        return Err(format!("quota '{value}' has no digits"));
    }
    let amount = digits
        .parse::<u64>()
        .map_err(|err| format!("parse quota '{value}': {err}"))?;
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| format!("quota '{value}' overflows u64"))
}

fn valid_mode(value: &str) -> bool {
    let len = value.len();
    (len == 3 || len == 4) && value.chars().all(|ch| matches!(ch, '0'..='7'))
}

fn valid_owner(value: &str) -> bool {
    let Some((uid, gid)) = value.split_once(':') else {
        return false;
    };
    !uid.is_empty()
        && !gid.is_empty()
        && uid.chars().all(|ch| ch.is_ascii_digit())
        && gid.chars().all(|ch| ch.is_ascii_digit())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServicePort {
    pub name: String,
    pub container_port: u16,
    #[serde(default = "PortProtocol::tcp")]
    pub protocol: PortProtocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublishedPort {
    pub service_port: String,
    pub host_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteSpec {
    Http(HttpRoute),
    Tcp(TcpRoute),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HttpRoute {
    pub service_port: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hostnames: Vec<String>,
    #[serde(default = "default_http_path_prefix")]
    pub path_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TcpRoute {
    pub service_port: String,
    pub listen_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadinessProbe {
    #[serde(flatten)]
    pub check: ReadinessCheck,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_period: Option<String>,
}

impl ReadinessProbe {
    pub fn validate(&self, service_name: &str) -> Result<(), String> {
        let interval = self
            .interval_duration()
            .map_err(|error| format!("service '{service_name}' readiness interval {error}"))?;
        if interval.is_zero() {
            return Err(format!(
                "service '{service_name}' readiness interval must be greater than zero"
            ));
        }

        let timeout = self
            .timeout_duration()
            .map_err(|error| format!("service '{service_name}' readiness timeout {error}"))?;
        if timeout.is_zero() {
            return Err(format!(
                "service '{service_name}' readiness timeout must be greater than zero"
            ));
        }

        self.start_period_duration()
            .map_err(|error| format!("service '{service_name}' readiness start_period {error}"))?;

        if self.retries() == 0 {
            return Err(format!(
                "service '{service_name}' readiness retries must be greater than zero"
            ));
        }

        Ok(())
    }

    pub fn interval_duration(&self) -> Result<Duration, String> {
        self.interval
            .as_deref()
            .map(parse_duration)
            .transpose()
            .map(|duration| duration.unwrap_or(Duration::from_secs(30)))
    }

    pub fn timeout_duration(&self) -> Result<Duration, String> {
        self.timeout
            .as_deref()
            .map(parse_duration)
            .transpose()
            .map(|duration| duration.unwrap_or(Duration::from_secs(30)))
    }

    pub fn start_period_duration(&self) -> Result<Duration, String> {
        self.start_period
            .as_deref()
            .map(parse_duration)
            .transpose()
            .map(|duration| duration.unwrap_or(Duration::ZERO))
    }

    #[must_use]
    pub fn retries(&self) -> u32 {
        self.retries.unwrap_or(3)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessCheck {
    Http { service_port: String, path: String },
    Tcp { service_port: String },
    Exec { command: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortProtocol {
    Tcp,
    Udp,
}

impl PortProtocol {
    #[must_use]
    pub fn tcp() -> Self {
        Self::Tcp
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PullPolicy {
    IfNotPresent,
    Always,
    Never,
}

impl PullPolicy {
    #[must_use]
    pub fn if_not_present() -> Self {
        Self::IfNotPresent
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RolloutStrategy {
    Recreate,
    BlueGreen,
}

impl RolloutStrategy {
    #[must_use]
    pub fn recreate() -> Self {
        Self::Recreate
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    UnlessStopped,
    Always,
    OnFailure,
    No,
}

impl RestartPolicy {
    #[must_use]
    pub fn unless_stopped() -> Self {
        Self::UnlessStopped
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_millicores: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
}

impl Resources {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            cpu_millicores: None,
            memory_bytes: None,
        }
    }

    #[must_use]
    pub fn cpu_nano(&self) -> Option<i64> {
        self.cpu_millicores.map(|m| i64::from(m) * 1_000_000)
    }
}

fn default_http_path_prefix() -> String {
    "/".into()
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("cannot be empty".into());
    }
    humantime_serde::re::humantime::parse_duration(trimmed)
        .map_err(|error| format!("must be a duration like '30s' or '5m', got '{value}': {error}"))
}

#[must_use]
pub fn stable_hash_hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;

    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }

    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> ServiceSpec {
        ServiceSpec {
            name: "api".into(),
            placement: Placement::Replicated { count: 1 },
            template: ContainerSpec {
                image: "myapp:latest".into(),
                command: Some(vec!["serve".into()]),
                entrypoint: None,
                env: BTreeMap::from([("PORT".into(), "8080".into())]),
                mounts: vec![Mount {
                    source: MountSource::Volume("data".into()),
                    target: "/data".into(),
                    readonly: false,
                }],
                cap_add: vec![],
                cap_drop: vec![],
                privileged: false,
                user: None,
                stop_grace_period: Some("10s".into()),
                pid_mode: Some("host".into()),
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources {
                    cpu_millicores: Some(1000),
                    memory_bytes: Some(512 * 1024 * 1024),
                },
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            service_ports: vec![ServicePort {
                name: "http".into(),
                container_port: 8080,
                protocol: PortProtocol::Tcp,
            }],
            publish: vec![],
            routes: vec![RouteSpec::Http(HttpRoute {
                service_port: "http".into(),
                hostnames: vec!["api.example.test".into()],
                path_prefix: "/".into(),
            })],
            readiness: Some(ReadinessProbe {
                check: ReadinessCheck::Http {
                    service_port: "http".into(),
                    path: "/ready".into(),
                },
                interval: None,
                timeout: None,
                retries: None,
                start_period: None,
            }),
            rollout: RolloutStrategy::BlueGreen,
            labels: BTreeMap::from([("env".into(), "prod".into())]),
            restart: RestartPolicy::UnlessStopped,
        }
    }

    #[test]
    fn serde_roundtrip() {
        let spec = sample_spec();
        let json = serde_json::to_string_pretty(&spec).expect("serialize spec");
        let deserialized: ServiceSpec = serde_json::from_str(&json).expect("deserialize spec");
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn revision_hash_is_stable() {
        let spec = sample_spec();
        let hash_a = spec.revision_hash().expect("hash a");
        let hash_b = spec.revision_hash().expect("hash b");
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn blue_green_requires_readiness() {
        let mut spec = sample_spec();
        spec.readiness = None;
        let error = spec.validate().expect_err("missing readiness should fail");
        assert!(error.contains("does not define readiness"));
    }

    #[test]
    fn host_ports_block_blue_green() {
        let mut spec = sample_spec();
        spec.publish.push(PublishedPort {
            service_port: "http".into(),
            host_port: 8080,
            host_ip: None,
        });
        let error = spec.validate().expect_err("publish should fail blue_green");
        assert!(error.contains("published host ports"));
    }

    #[test]
    fn manifest_rejects_duplicate_services() {
        let spec = sample_spec();
        let manifest = DeployManifest {
            namespace: Namespace::default_ns(),
            intent: None,
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![spec.clone(), spec],
        };
        let error = manifest.validate().expect_err("duplicates should fail");
        assert!(error.contains("duplicate service"));
    }

    #[test]
    fn manifest_accepts_empty_services() {
        let manifest = DeployManifest {
            namespace: Namespace::default_ns(),
            intent: None,
            volumes: Vec::new(),
            services: Vec::new(),
        };

        manifest
            .validate()
            .expect("empty services means remove current releases");
    }

    #[test]
    fn deploy_intent_roundtrips_in_manifest_json() {
        let manifest = DeployManifest {
            namespace: Namespace("pr_39".into()),
            intent: Some(DeployIntent {
                services: vec![ServiceIntentHint {
                    service: "api".into(),
                    intent: ServiceIntent::Branch {
                        source_namespace: Namespace("prod".into()),
                        source_service: "api".into(),
                    },
                }],
                volumes: vec![VolumeIntentHint {
                    volume: "data".into(),
                    intent: VolumeIntent::Move {
                        from_machine: "machine-a".into(),
                        to_machine: "machine-b".into(),
                    },
                }],
                phases: Vec::new(),
            }),
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![sample_spec()],
        };

        let json = serde_json::to_string(&manifest).expect("serialize manifest");
        assert!(json.contains("\"intent\""));
        assert!(json.contains("\"branch\""));
        assert!(json.contains("\"move\""));

        let deserialized: DeployManifest =
            serde_json::from_str(&json).expect("deserialize manifest");
        assert_eq!(deserialized, manifest);
    }

    #[test]
    fn deploy_intent_accepts_service_branch_planner_support() {
        let manifest = DeployManifest {
            namespace: Namespace("pr_39".into()),
            intent: Some(DeployIntent {
                services: vec![ServiceIntentHint {
                    service: "api".into(),
                    intent: ServiceIntent::Branch {
                        source_namespace: Namespace("prod".into()),
                        source_service: "api".into(),
                    },
                }],
                volumes: Vec::new(),
                phases: Vec::new(),
            }),
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![sample_spec()],
        };

        manifest
            .validate()
            .expect("service branch intent is supported by deploy planning");
    }

    #[test]
    fn deploy_intent_rejects_unplanned_modes_after_shape_validation() {
        let mut manifest = DeployManifest {
            namespace: Namespace("pr_39".into()),
            intent: Some(DeployIntent {
                services: vec![ServiceIntentHint {
                    service: "api".into(),
                    intent: ServiceIntent::Portal {
                        source_namespace: Namespace("prod".into()),
                        source_service: "api".into(),
                    },
                }],
                volumes: Vec::new(),
                phases: Vec::new(),
            }),
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![sample_spec()],
        };

        let error = manifest
            .validate()
            .expect_err("portal should be rejected until deploy planning supports it");
        assert!(
            error.contains("portal for service 'api' is not supported"),
            "got: {error}"
        );

        manifest.intent = Some(DeployIntent {
            services: Vec::new(),
            volumes: vec![VolumeIntentHint {
                volume: "data".into(),
                intent: VolumeIntent::Move {
                    from_machine: "machine-a".into(),
                    to_machine: "machine-b".into(),
                },
            }],
            phases: Vec::new(),
        });

        manifest
            .validate()
            .expect("volume move intent is supported by deploy planning");
    }

    #[test]
    fn deploy_intent_validates_shape_before_rejecting_unsupported_modes() {
        let manifest = DeployManifest {
            namespace: Namespace("pr_39".into()),
            intent: Some(DeployIntent {
                services: vec![ServiceIntentHint {
                    service: "api".into(),
                    intent: ServiceIntent::Branch {
                        source_namespace: Namespace("Prod".into()),
                        source_service: "api".into(),
                    },
                }],
                volumes: Vec::new(),
                phases: Vec::new(),
            }),
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![sample_spec()],
        };

        let error = manifest
            .validate()
            .expect_err("invalid source namespace should fail before unsupported mode");

        assert!(error.contains("invalid source namespace"), "got: {error}");
    }

    #[test]
    fn deploy_intent_rejects_unknown_nested_fields() {
        let json = r#"{
            "namespace":"pr_39",
            "intent":{
                "services":[
                    {
                        "service":"api",
                        "intent":{
                            "branch":{
                                "source_namespace":"prod",
                                "source_service":"api",
                                "snapshot_policy":"clone"
                            }
                        }
                    }
                ]
            },
            "volumes":[
                {
                    "name":"data",
                    "scope":"single",
                    "quota":"10G",
                    "mode":"0750",
                    "owner":"999:999"
                }
            ],
            "services":[]
        }"#;

        let error =
            serde_json::from_str::<DeployManifest>(json).expect_err("unknown field should fail");

        assert!(error.to_string().contains("snapshot_policy"));
    }

    #[test]
    fn deploy_intent_rejects_unknown_targets() {
        let manifest = DeployManifest {
            namespace: Namespace::default_ns(),
            intent: Some(DeployIntent {
                services: vec![ServiceIntentHint {
                    service: "worker".into(),
                    intent: ServiceIntent::Move {
                        to_machine: "machine-b".into(),
                    },
                }],
                volumes: vec![VolumeIntentHint {
                    volume: "cache".into(),
                    intent: VolumeIntent::Move {
                        from_machine: "machine-a".into(),
                        to_machine: "machine-b".into(),
                    },
                }],
                phases: Vec::new(),
            }),
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![sample_spec()],
        };

        let error = manifest
            .validate()
            .expect_err("unknown service should fail before unsupported mode");

        assert!(error.contains("unknown service"), "got: {error}");
    }

    #[test]
    fn deploy_intent_accepts_ordered_checkpoint_and_no_store_phases() {
        let manifest = DeployManifest {
            namespace: Namespace::default_ns(),
            intent: Some(DeployIntent {
                services: Vec::new(),
                volumes: Vec::new(),
                phases: vec![
                    DeployPhaseIntent {
                        phase_id: "schema".into(),
                        name: Some("Schema".into()),
                        after: Vec::new(),
                        services: vec!["api".into()],
                        volumes: Vec::new(),
                        commit_policy: DeployPhaseCommitPolicy::Checkpoint,
                        rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
                        advance_policy: DeployPhaseAdvancePolicy::Immediate,
                    },
                    DeployPhaseIntent {
                        phase_id: "observe".into(),
                        name: None,
                        after: vec!["schema".into()],
                        services: Vec::new(),
                        volumes: Vec::new(),
                        commit_policy: DeployPhaseCommitPolicy::NoStoreCommit,
                        rollback_policy: DeployPhaseRollbackPolicy::External,
                        advance_policy: DeployPhaseAdvancePolicy::Immediate,
                    },
                ],
            }),
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![sample_spec()],
        };

        manifest
            .validate()
            .expect("checkpoint and empty no-store phases should validate");
    }

    #[test]
    fn deploy_intent_rejects_reversible_checkpoint_and_cycles() {
        let mut manifest = DeployManifest {
            namespace: Namespace::default_ns(),
            intent: Some(DeployIntent {
                services: Vec::new(),
                volumes: Vec::new(),
                phases: vec![DeployPhaseIntent {
                    phase_id: "schema".into(),
                    name: None,
                    after: Vec::new(),
                    services: vec!["api".into()],
                    volumes: Vec::new(),
                    commit_policy: DeployPhaseCommitPolicy::Checkpoint,
                    rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                    advance_policy: DeployPhaseAdvancePolicy::Immediate,
                }],
            }),
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![sample_spec()],
        };

        let error = manifest
            .validate()
            .expect_err("reversible checkpoint should fail");
        assert!(
            error.contains("non-reversible rollback policy"),
            "got: {error}"
        );

        manifest.intent = Some(DeployIntent {
            services: Vec::new(),
            volumes: Vec::new(),
            phases: vec![
                DeployPhaseIntent {
                    phase_id: "a".into(),
                    name: None,
                    after: vec!["b".into()],
                    services: vec!["api".into()],
                    volumes: Vec::new(),
                    commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                    rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                    advance_policy: DeployPhaseAdvancePolicy::Immediate,
                },
                DeployPhaseIntent {
                    phase_id: "b".into(),
                    name: None,
                    after: vec!["a".into()],
                    services: Vec::new(),
                    volumes: Vec::new(),
                    commit_policy: DeployPhaseCommitPolicy::NoStoreCommit,
                    rollback_policy: DeployPhaseRollbackPolicy::External,
                    advance_policy: DeployPhaseAdvancePolicy::Immediate,
                },
            ],
        });

        let error = manifest.validate().expect_err("phase cycle should fail");
        assert!(error.contains("cycle"), "got: {error}");
    }

    #[test]
    fn deploy_intent_rejects_no_store_phase_with_resource_work() {
        let manifest = DeployManifest {
            namespace: Namespace::default_ns(),
            intent: Some(DeployIntent {
                services: Vec::new(),
                volumes: Vec::new(),
                phases: vec![DeployPhaseIntent {
                    phase_id: "observe".into(),
                    name: None,
                    after: Vec::new(),
                    services: vec!["api".into()],
                    volumes: Vec::new(),
                    commit_policy: DeployPhaseCommitPolicy::NoStoreCommit,
                    rollback_policy: DeployPhaseRollbackPolicy::External,
                    advance_policy: DeployPhaseAdvancePolicy::Immediate,
                }],
            }),
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![sample_spec()],
        };

        let error = manifest
            .validate()
            .expect_err("no-store phase with resource work should fail");
        assert!(
            error.contains("cannot reference services or volumes"),
            "got: {error}"
        );
    }

    #[test]
    fn manifest_rejects_empty_namespace() {
        let manifest = DeployManifest {
            namespace: Namespace(String::new()),
            intent: None,
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![sample_spec()],
        };
        let error = manifest
            .validate()
            .expect_err("empty namespace should fail");
        assert!(error.contains("namespace"));
    }

    #[test]
    fn manifest_rejects_path_segment_namespace() {
        for namespace in ["../tmp", "prod/api", "prod api", ".prod", "Prod"] {
            let manifest = DeployManifest {
                namespace: Namespace(namespace.into()),
                intent: None,
                volumes: vec![VolumeDeclaration {
                    name: "data".into(),
                    scope: VolumeScope::Single,
                    quota: "10G".into(),
                    mode: "0750".into(),
                    owner: "999:999".into(),
                }],
                services: vec![sample_spec()],
            };

            let error = manifest
                .validate()
                .expect_err("path-segment namespace should fail");

            assert!(error.contains("namespace"));
        }
    }

    #[test]
    fn volume_declaration_rejects_path_segment_name() {
        for name in ["../tmp", "data/extra", "data extra", ".data", "Data"] {
            let manifest = DeployManifest {
                namespace: Namespace::default_ns(),
                intent: None,
                volumes: vec![VolumeDeclaration {
                    name: name.into(),
                    scope: VolumeScope::Single,
                    quota: "10G".into(),
                    mode: "0750".into(),
                    owner: "999:999".into(),
                }],
                services: vec![sample_spec()],
            };

            let error = manifest
                .validate()
                .expect_err("path-segment volume name should fail");

            assert!(error.contains("volume name"), "got: {error}");
        }
    }

    #[test]
    fn replicated_zero_is_rejected() {
        let mut spec = sample_spec();
        spec.placement = Placement::Replicated { count: 0 };
        let error = spec.validate().expect_err("zero replicas should fail");
        assert!(error.contains("at least one replica"));
    }

    #[test]
    fn single_scope_volume_rejects_multiple_replicas() {
        let mut spec = sample_spec();
        spec.placement = Placement::Replicated { count: 2 };
        let manifest = DeployManifest {
            namespace: Namespace::default_ns(),
            intent: None,
            volumes: vec![VolumeDeclaration {
                name: "data".into(),
                scope: VolumeScope::Single,
                quota: "10G".into(),
                mode: "0750".into(),
                owner: "999:999".into(),
            }],
            services: vec![spec],
        };

        let error = manifest.validate().expect_err("single volume should fail");

        assert!(error.contains("replicas > 1"));
        assert!(error.contains("scope=single"));
    }

    #[test]
    fn bind_mount_is_rejected_outside_system_namespace() {
        let mut spec = sample_spec();
        spec.template.mounts = vec![Mount {
            source: MountSource::Bind("/host/data".into()),
            target: "/data".into(),
            readonly: false,
        }];
        spec.rollout = RolloutStrategy::Recreate;
        let manifest = DeployManifest {
            namespace: Namespace::default_ns(),
            intent: None,
            volumes: Vec::new(),
            services: vec![spec],
        };

        let error = manifest.validate().expect_err("bind mount should fail");

        assert!(error.contains("bind mount"));
        assert!(error.contains("system"));
    }

    #[test]
    fn readiness_accepts_compose_style_timing() {
        let mut spec = sample_spec();
        spec.readiness = Some(ReadinessProbe {
            check: ReadinessCheck::Http {
                service_port: "http".into(),
                path: "/ready".into(),
            },
            interval: Some("5s".into()),
            timeout: Some("2s".into()),
            retries: Some(60),
            start_period: Some("10s".into()),
        });
        spec.validate()
            .expect("compose style readiness should validate");
    }

    #[test]
    fn readiness_rejects_invalid_timing() {
        let mut spec = sample_spec();
        spec.readiness = Some(ReadinessProbe {
            check: ReadinessCheck::Tcp {
                service_port: "http".into(),
            },
            interval: Some("0s".into()),
            timeout: None,
            retries: None,
            start_period: None,
        });
        let error = spec.validate().expect_err("zero interval should fail");
        assert!(error.contains("interval must be greater than zero"));
    }

    #[test]
    fn old_spec_json_with_namespace_still_deserializes() {
        let json = r#"{
            "name":"api",
            "namespace":"prod",
            "placement":{"replicated":{"count":2}},
            "template":{"image":"myapp:latest","command":["serve"],"env":{"PORT":"8080"},"volumes":[],"cap_add":[],"cap_drop":[],"privileged":false,"stop_grace_period":"10s","pull_policy":"if_not_present","resources":{"cpu_millicores":1000,"memory_bytes":536870912},"sysctls":{}},
            "network":"overlay",
            "service_ports":[{"name":"http","container_port":8080,"protocol":"tcp"}],
            "publish":[],
            "routes":[{"http":{"service_port":"http","hostnames":["api.example.test"],"path_prefix":"/"}}],
            "readiness":{"http":{"service_port":"http","path":"/ready"}},
            "rollout":"blue_green",
            "labels":{"env":"prod"},
            "restart":"unless-stopped"
        }"#;

        let spec: ServiceSpec = serde_json::from_str(json).expect("deserialize legacy spec");
        assert_eq!(spec.name, "api");
        assert_eq!(spec.network, NetworkMode::Overlay);
    }

    #[test]
    fn legacy_template_volumes_deserialize_as_mounts() {
        let json = r#"{
            "image":"myapp:latest",
            "volumes":[
                {
                    "source":{"bind":"/host/data"},
                    "target":"/data",
                    "readonly":true
                },
                {
                    "source":"tmpfs",
                    "target":"/cache"
                }
            ]
        }"#;

        let template: ContainerSpec =
            serde_json::from_str(json).expect("deserialize legacy template volumes");

        assert_eq!(
            template.mounts,
            vec![
                Mount {
                    source: MountSource::Bind("/host/data".into()),
                    target: "/data".into(),
                    readonly: true,
                },
                Mount {
                    source: MountSource::Tmpfs,
                    target: "/cache".into(),
                    readonly: false,
                },
            ]
        );
    }

    #[test]
    fn readiness_json_roundtrips_with_timing_fields() {
        let json = r#"{
            "http":{"service_port":"http","path":"/ready"},
            "interval":"5s",
            "timeout":"2s",
            "retries":60,
            "start_period":"10s"
        }"#;

        let readiness: ReadinessProbe = serde_json::from_str(json).expect("deserialize readiness");
        assert_eq!(
            readiness,
            ReadinessProbe {
                check: ReadinessCheck::Http {
                    service_port: "http".into(),
                    path: "/ready".into()
                },
                interval: Some("5s".into()),
                timeout: Some("2s".into()),
                retries: Some(60),
                start_period: Some("10s".into()),
            }
        );
    }

    #[test]
    fn empty_pid_mode_is_rejected() {
        let mut spec = sample_spec();
        spec.template.pid_mode = Some("   ".into());
        let error = spec.validate().expect_err("empty pid mode should fail");
        assert!(error.contains("pid_mode"));
    }

    #[test]
    fn pid_mode_roundtrips_in_template_json() {
        let spec = sample_spec();
        let json = serde_json::to_string(&spec).expect("serialize spec");
        assert!(json.contains("\"pid_mode\":\"host\""));

        let deserialized: ServiceSpec = serde_json::from_str(&json).expect("deserialize spec");
        assert_eq!(deserialized.template.pid_mode.as_deref(), Some("host"));
    }

    #[test]
    fn parse_quota_bytes_accepts_decimal_and_suffixes() {
        let cases: &[(&str, u64)] = &[
            ("1024", 1024),
            ("1K", 1024),
            ("1M", 1024_u64.pow(2)),
            ("2G", 2 * 1024_u64.pow(3)),
            ("1T", 1024_u64.pow(4)),
        ];
        for (input, expected) in cases {
            let actual = parse_quota_bytes(input)
                .unwrap_or_else(|err| panic!("parse_quota_bytes({input}) failed: {err}"));
            assert_eq!(actual, *expected, "input {input}");
        }
    }

    #[test]
    fn parse_quota_bytes_rejects_empty() {
        let error = parse_quota_bytes("").expect_err("empty quota should fail");
        assert!(error.contains("empty"), "got: {error}");
    }

    #[test]
    fn parse_quota_bytes_rejects_no_digits() {
        let error = parse_quota_bytes("K").expect_err("digit-less quota should fail");
        assert!(error.contains("no digits"), "got: {error}");
    }

    #[test]
    fn parse_quota_bytes_rejects_unknown_suffix() {
        let error = parse_quota_bytes("5P").expect_err("P suffix is not supported");
        assert!(error.contains("unsupported quota suffix"), "got: {error}");
    }

    #[test]
    fn parse_quota_bytes_overflow() {
        // 1024^4 = 2^40, so any amount >= 2^24 = 16_777_216 overflows u64 when
        // multiplied by the T suffix.
        let error = parse_quota_bytes("16777216T").expect_err("16777216T overflows u64");
        assert!(error.contains("overflows"), "got: {error}");
    }

    #[test]
    fn parse_quota_bytes_just_below_overflow_succeeds() {
        let actual = parse_quota_bytes("16777215T").expect("16777215T fits in u64");
        assert_eq!(actual, 16_777_215_u64 * 1024_u64.pow(4));
    }
}
