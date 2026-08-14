use std::{
    ffi::OsStr,
    net::Ipv4Addr,
    process::{Output, Stdio},
};

use oci_spec::distribution::Reference;
use ployz_core::{
    Machine, MachineFailure, MachineSelector, MachineSuccess, PartialResult, UNREGISTRY_PORT,
    resolve_machine_selectors,
};
use thiserror::Error;
use tokio::process::Command;

use crate::connect::Client;

use self::proxy::{ImageProxy, ProxyMode, detect_mode};

mod proxy;

#[derive(Debug, Error)]
pub enum PushError {
    #[error("invalid image reference '{reference}': {message}")]
    InvalidReference { reference: String, message: String },
    #[error("direct image push requires a tagged local reference")]
    DigestReference,
    #[error("direct image push cannot preserve registry-with-port reference '{0}'")]
    RegistryPortReference(String),
    #[error("unsupported platform '{0}'")]
    UnsupportedPlatform(String),
    #[error("image push cancelled")]
    Cancelled,
    #[error("listen for image-push cancellation: {0}")]
    Cancellation(#[source] std::io::Error),
    #[error("image '{0}' not found locally")]
    ImageNotFound(String),
    #[error("Machine target selection failed: {0}")]
    TargetSelection(String),
    #[error("Cluster operation failed: {0}")]
    Cluster(String),
    #[error("Docker {action}: {diagnostic}")]
    Docker {
        action: &'static str,
        diagnostic: String,
    },
    #[error("image proxy {action}: {diagnostic}")]
    Proxy {
        action: &'static str,
        diagnostic: String,
    },
    #[error("Docker is not using the required containerd image store")]
    UnsupportedImageStore,
    #[error("unsupported Machine subnet {0}")]
    UnsupportedSubnet(String),
    #[error("image-push cleanup failed: {0}")]
    Cleanup(String),
    #[error("{primary}; cleanup: {cleanup}")]
    CleanupAfter {
        primary: Box<PushError>,
        cleanup: Box<PushError>,
    },
    #[error("{machine}: {source}")]
    Machine {
        machine: String,
        #[source]
        source: Box<PushError>,
    },
}

pub async fn push(
    client: &mut Client,
    image: &str,
    platform: Option<&str>,
    selectors: &[String],
) -> Result<PartialResult<(), PushError>, PushError> {
    // TODO(UT-022): without an explicit platform, Docker chooses what to push; target platforms are not inferred.
    let platform = platform.map(validated_platform).transpose()?;
    let reference = tagged_reference(image)?;
    let inspected = docker_output(["image", "inspect", image]).await?;
    if !inspected.status.success() {
        return Err(if not_found(&inspected) {
            PushError::ImageNotFound(image.into())
        } else {
            command_error("inspect local image", &inspected)
        });
    }
    let targets = select_targets(
        &client
            .list_machines()
            .await
            .map_err(|error| PushError::Cluster(error.to_string()))?,
        selectors,
    )?;
    let mode = detect_mode().await?;
    let mut result = PartialResult {
        successes: Vec::new(),
        failures: Vec::new(),
        omissions: Vec::new(),
    };
    for machine in targets {
        match push_to_machine(client, image, platform, &reference, &machine, mode).await {
            Ok(()) => result.successes.push(MachineSuccess {
                machine_id: machine.id,
                value: (),
            }),
            Err(source) => result.failures.push(MachineFailure {
                machine_id: machine.id,
                error: PushError::Machine {
                    machine: machine.name.to_string(),
                    source: Box::new(source),
                },
            }),
        }
    }
    Ok(result)
}

fn select_targets(
    observations: &[ployz_core::MachineObservation],
    selectors: &[String],
) -> Result<Vec<Machine>, PushError> {
    let machines = observations
        .iter()
        .map(|observation| observation.machine.clone())
        .collect::<Vec<_>>();
    let selectors = if selectors.is_empty() {
        vec![MachineSelector::parse("*").expect("wildcard selector is valid")]
    } else {
        selectors
            .iter()
            .map(MachineSelector::parse)
            .collect::<Result<_, _>>()
            .map_err(|error| PushError::TargetSelection(error.to_string()))?
    };
    resolve_machine_selectors(&machines, &selectors)
        .map_err(|error| PushError::TargetSelection(error.to_string()))
}

async fn push_to_machine(
    client: &Client,
    image: &str,
    platform: Option<&str>,
    reference: &Reference,
    machine: &Machine,
    mode: ProxyMode,
) -> Result<(), PushError> {
    let store = client
        .list_images(Some(image.into()), &[machine.id.to_string()])
        .await
        .map_err(|error| PushError::Cluster(format!("check image store: {error}")))?;
    let store = store.successes.first().ok_or_else(|| {
        PushError::Cluster(
            store
                .failures
                .first()
                .map(|failure| failure.error.message.clone())
                .unwrap_or_else(|| "target returned no image-store result".into()),
        )
    })?;
    if !store.value.images.containerd_store {
        return Err(PushError::UnsupportedImageStore);
    }
    let network = machine.subnet.0;
    if network.prefix_len() != 24 {
        return Err(PushError::UnsupportedSubnet(network.to_string()));
    }
    let gateway = Ipv4Addr::from(u32::from(network.network()) + 1);
    let remote = format!("{gateway}:{UNREGISTRY_PORT}");
    client
        .dial_proxy("tcp", &remote)
        .await
        .map_err(|error| PushError::Cluster(format!("reach unregistry: {error}")))?;
    PushSession::run(client, remote, mode, image, platform, reference).await
}

struct PushSession {
    proxy: ImageProxy,
    temporary: Option<String>,
}

impl PushSession {
    async fn run(
        client: &Client,
        remote: String,
        mode: ProxyMode,
        image: &str,
        platform: Option<&str>,
        reference: &Reference,
    ) -> Result<(), PushError> {
        let mut session = Self {
            proxy: ImageProxy::open(mode).await?,
            temporary: None,
        };
        let outcome = tokio::select! {
            outcome = session.push(client, remote, image, platform, reference) => outcome,
            error = cancellation() => Err(error),
        };
        let cleanup = session.cleanup().await;
        match (outcome, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(PushError::CleanupAfter {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }),
        }
    }

    async fn push(
        &mut self,
        client: &Client,
        remote: String,
        image: &str,
        platform: Option<&str>,
        reference: &Reference,
    ) -> Result<(), PushError> {
        let temporary = temporary_reference(self.proxy.push_port(), reference);
        let tagged = docker_output(["tag", image, &temporary]).await?;
        if !tagged.status.success() {
            return Err(command_error("tag image for push", &tagged));
        }
        self.temporary = Some(temporary.clone());
        let push = async {
            let mut command = Command::new("docker");
            command.arg("push");
            if let Some(platform) = platform {
                command.args(["--platform", platform]);
            }
            let status = command
                .arg(&temporary)
                .kill_on_drop(true)
                .status()
                .await
                .map_err(|error| PushError::Docker {
                    action: "push",
                    diagnostic: error.to_string(),
                })?;
            status.success().then_some(()).ok_or(PushError::Docker {
                action: "push",
                diagnostic: format!("exited with {status}"),
            })
        };
        // TODO(UT-023): direct push keeps Docker's progress stream; no quiet mode is exposed.
        tokio::select! {
            outcome = push => outcome,
            outcome = self.proxy.serve(client.clone(), remote) => outcome,
        }
    }

    async fn cleanup(&mut self) -> Result<(), PushError> {
        let mut errors = Vec::new();
        if let Some(temporary) = &self.temporary
            && let Err(error) = remove_image(temporary).await
        {
            errors.push(error.to_string());
        }
        if let Err(error) = self.proxy.cleanup().await {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(PushError::Cleanup(errors.join("; ")))
        }
    }
}

async fn cancellation() -> PushError {
    match tokio::signal::ctrl_c().await {
        Ok(()) => PushError::Cancelled,
        Err(error) => PushError::Cancellation(error),
    }
}

fn tagged_reference(image: &str) -> Result<Reference, PushError> {
    let reference = image
        .parse::<Reference>()
        .map_err(|error| PushError::InvalidReference {
            reference: image.into(),
            message: error.to_string(),
        })?;
    if reference.digest().is_some() {
        return Err(PushError::DigestReference);
    }
    if reference.registry().contains(':') {
        return Err(PushError::RegistryPortReference(image.into()));
    }
    Ok(reference)
}

fn temporary_reference(port: u16, reference: &Reference) -> String {
    format!(
        "127.0.0.1:{port}/{}/{}:{}",
        reference.registry(),
        reference.repository(),
        reference.tag().expect("digest references were rejected")
    )
}

fn validated_platform(platform: &str) -> Result<&str, PushError> {
    match platform {
        "linux/amd64" | "linux/arm64" => Ok(platform),
        _ => Err(PushError::UnsupportedPlatform(platform.into())),
    }
}

async fn remove_image(image: &str) -> Result<(), PushError> {
    let output = docker_output(["image", "rm", image]).await?;
    if output.status.success() || not_found(&output) {
        Ok(())
    } else {
        Err(command_error("remove temporary image", &output))
    }
}

async fn docker_output<I, S>(args: I) -> Result<Output, PushError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| PushError::Docker {
            action: "run command",
            diagnostic: error.to_string(),
        })
}

fn command_error(action: &'static str, output: &Output) -> PushError {
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    PushError::Docker {
        action,
        diagnostic: diagnostic.trim().into(),
    }
}

fn not_found(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr)
        .to_ascii_lowercase()
        .contains("no such")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{
        MachineId, MachineName, MachineObservation, MachineSubnet, ManagementAddress,
        MembershipObservation, WireGuardPublicKey,
    };

    fn machine(seed: u8) -> MachineObservation {
        MachineObservation {
            machine: Machine {
                id: MachineId::parse(format!("{seed:032x}")).unwrap(),
                name: MachineName::parse(format!("machine-{seed}")).unwrap(),
                subnet: MachineSubnet(format!("10.210.{seed}.0/24").parse().unwrap()),
                management_address: ManagementAddress("fd00::1".parse().unwrap()),
                public_key: WireGuardPublicKey([seed; 32]),
                advertised_endpoints: Vec::new(),
            },
            membership: MembershipObservation::Up,
            selected_endpoint: None,
        }
    }

    #[test]
    fn target_and_proxy_selection_preserve_the_explicit_contract() {
        let machines = [machine(1), machine(2)];
        assert_eq!(select_targets(&machines, &[]).unwrap().len(), 2);
        assert_eq!(
            select_targets(&machines, &["machine-2".into()])
                .unwrap()
                .first()
                .unwrap()
                .name
                .as_str(),
            "machine-2"
        );
        assert_eq!(select_targets(&machines, &["all".into()]).unwrap().len(), 2);
        assert!(select_targets(&machines, &["missing".into()]).is_err());
        assert_eq!(proxy::mode_for(false, false), ProxyMode::Native);
        assert_eq!(proxy::mode_for(false, true), ProxyMode::Rootless);
        assert_eq!(proxy::mode_for(true, false), ProxyMode::Vm);
        assert_eq!(proxy::mode_for(true, true), ProxyMode::Vm);
        let reference = temporary_reference(
            5000,
            &tagged_reference("registry.test/team/api:v1").unwrap(),
        );
        assert_eq!(reference, "127.0.0.1:5000/registry.test/team/api:v1");
        assert!(matches!(
            tagged_reference("localhost:5000/team/api:v1"),
            Err(PushError::RegistryPortReference(_))
        ));
        assert!(tagged_reference("registry.test/team/api@sha256:abc").is_err());
        assert!(validated_platform("linux/riscv64").is_err());
    }
}
