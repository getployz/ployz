//! Deliver a completed Build from the Machine that holds its exact content.

use ployz_build::BuiltImage;
use ployz_core::{ListImagesRequest, MachineObservation};

use super::*;

/// Transfer the completed image directly from its Build Machine. The client
/// neither inspects Docker nor carries image bytes or selects another source.
/// # Errors
/// Reports selection, source capability, and source image-store failures.
/// Destination failures and unattempted destinations remain in the partial result.
pub async fn push_from_machine(
    client: &mut Client,
    image: &BuiltImage,
    source: MachineId,
    selectors: &[String],
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<PartialResult<(), PushError>, PushError> {
    let machines = Cancellation::new(cancellation)
        .race(client.machines())
        .await??;
    push_from_machine_using_machines(client, image, source, selectors, &machines, cancellation)
        .await
}

pub(crate) async fn push_from_machine_using_machines(
    client: &mut Client,
    image: &BuiltImage,
    source: MachineId,
    selectors: &[String],
    machines: &[MachineObservation],
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<PartialResult<(), PushError>, PushError> {
    let selection = list_selection(machines, selectors)?;
    let mut result = PartialResult {
        successes: Vec::new(),
        failures: Vec::new(),
        omissions: selection.omissions,
    };
    if selection.targets.is_empty() {
        return Ok(result);
    }
    let source = serve_build_image(client, image, source, cancellation).await?;
    let reference = image
        .repository_reference()
        .map_err(|error| PushError::InvalidReference {
            reference: image.reference.clone(),
            message: error.to_string(),
        })?;
    let mut cancellation = Cancellation::new(cancellation);
    let mut remaining = selection.targets.into_iter();
    while let Some(machine) = remaining.next() {
        match pull_on_machine(client, &reference, &machine, source, &mut cancellation).await {
            Ok(()) => result.successes.push(MachineSuccess {
                machine_id: machine.id,
                value: (),
            }),
            Err(error) => {
                let cancelled = error.is_cancellation();
                result.failures.push(MachineFailure {
                    machine_id: machine.id,
                    error,
                });
                if cancelled {
                    result.omissions.extend(remaining.map(|machine| machine.id));
                    break;
                }
            }
        }
    }
    Ok(result)
}

/// Open the actual complete Build source for either delivery or a later Build.
pub(crate) async fn serve_build_image(
    client: &mut Client,
    image: &BuiltImage,
    source: MachineId,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<ImageIngestDestination, PushError> {
    let mut cancellation = Cancellation::new(cancellation);
    let digest = &image.reference;
    let target = MachineTarget::from(&source);
    // Docker's reference filter does not match repository@digest. Compare the
    // manifest identity and available platforms in the source's actual store.
    let stored = cancellation
        .race(client.call::<op::ListImages>(ListImagesRequest { reference: None }, Some(&target)))
        .await??;
    if !stored.containerd_store {
        return Err(PushError::UnsupportedImageStore);
    }
    if !stored.images.iter().any(|stored| {
        &stored.id == digest
            && image.platforms.iter().all(|expected| {
                stored.platforms.iter().any(|platform| {
                    platform == expected
                        || matches!(
                            (platform.as_str(), expected.as_str()),
                            ("linux/arm64", "linux/arm64/v8") | ("linux/arm64/v8", "linux/arm64")
                        )
                })
            })
    }) {
        return Err(PushError::BuildImageUnavailable {
            image: image.reference.clone(),
            machine_id: source,
            platform: image.platforms.join(", "),
        });
    }
    let opened = cancellation
        .race(client.call::<op::EnsureImageIngest>(EnsureImageIngestRequest {}, Some(&target)))
        .await?
        .map_err(|error| ingest_error(rpc_error(error)))?;
    Ok(opened.destination)
}

/// Compare completed image platforms against the destination's reported architecture.
pub(crate) fn platform_compatible(platform: &str, architecture: &str) -> bool {
    let (architecture, arm_generation) = match architecture {
        "x86_64" => ("amd64", None),
        "x86" | "i386" | "i486" | "i586" | "i686" => ("386", None),
        "aarch64" => ("arm64", None),
        "armv5l" => ("arm", Some(5)),
        "armv6l" => ("arm", Some(6)),
        "armv7l" => ("arm", Some(7)),
        "armv8l" => ("arm", Some(8)),
        "powerpc" => ("ppc", None),
        "powerpc64" => ("ppc64", None),
        "mipsel" => ("mipsle", None),
        "mips64el" => ("mips64le", None),
        "loongarch64" => ("loong64", None),
        other => (other, None),
    };
    let Some(platform) = platform.strip_prefix("linux/") else {
        return false;
    };
    let (image_architecture, variant) = platform
        .split_once('/')
        .map_or((platform, None), |(architecture, variant)| {
            (architecture, Some(variant))
        });
    if image_architecture != architecture {
        return false;
    }
    match (architecture, variant) {
        (_, None) | ("amd64", Some("v1")) | ("arm64", Some("v8")) => true,
        ("arm", Some(variant)) => variant
            .strip_prefix('v')
            .and_then(|value| value.parse::<u8>().ok())
            .is_some_and(|generation| (5..=arm_generation.unwrap_or(8)).contains(&generation)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn platform_compatibility_preserves_known_arm_generations() {
        for (platform, architecture, compatible) in [
            ("linux/arm/v7", "armv6l", false),
            ("linux/arm/v7", "armv7l", true),
            ("linux/arm/v6", "armv7l", true),
            ("linux/arm/v7", "arm", true),
            ("linux/arm/v7/extra", "armv7l", false),
            ("linux/arm64/v8", "aarch64", true),
            ("linux/amd64/v3", "x86_64", false),
            ("linux/386", "i686", true),
            ("linux/ppc64le", "powerpc64", false),
            ("linux/ppc64le", "ppc64le", true),
            ("linux/mips64le", "mips64el", true),
            ("linux/loong64", "loongarch64", true),
        ] {
            assert_eq!(
                super::platform_compatible(platform, architecture),
                compatible,
                "{platform} on {architecture}"
            );
        }
    }
}
