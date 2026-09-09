//! Deliver a completed Build from the Machine that holds its exact content.
//!
//! Every image source is selected by one rule, [`available_variant`]: a
//! Machine serves a destination only the variant its store demonstrably holds
//! for that destination. Build delivery, the local push peer hop, and Deploy's
//! peer lookup all route through it.

use ployz_build::BuiltImage;
use ployz_core::{ImageSummary, ListImagesRequest, MachineObservation};

use super::*;

/// Transfer the completed image directly from its Build Machine. The client
/// neither inspects Docker nor carries image bytes or selects another source.
/// # Errors
/// Reports selection, source capability, and source image-store failures.
/// Destination failures and unattempted destinations remain in the partial result.
pub async fn push_from_machine(
    client: &mut Client,
    image: &BuiltImage,
    repository: &str,
    source: MachineId,
    selectors: &[String],
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<PartialResult<(), PushError>, PushError> {
    let machines = Cancellation::new(cancellation)
        .race(client.machines())
        .await??;
    push_from_machine_using_machines(
        client,
        image,
        repository,
        source,
        selectors,
        &machines,
        cancellation,
    )
    .await
}

pub(crate) async fn push_from_machine_using_machines(
    client: &mut Client,
    image: &BuiltImage,
    repository: &str,
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
    let mut cancellation = Cancellation::new(cancellation);
    let source = Source::open(client, source, &mut cancellation).await?;
    source.require_complete(image)?;
    let reference =
        image
            .repository_reference(repository)
            .map_err(|error| PushError::InvalidReference {
                reference: image.reference.clone(),
                message: error.to_string(),
            })?;
    let mut remaining = selection.targets.into_iter();
    while let Some(machine) = remaining.next() {
        let delivery = source
            .deliver(
                client,
                ImageContent::built(&reference, &image.reference),
                &machine,
                None,
                &mut cancellation,
            )
            .await;
        match delivery {
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
///
/// # Errors
/// Reports an unusable store, a partial Build host, and a failed ingest start.
pub(crate) async fn serve_build_image(
    client: &mut Client,
    image: &BuiltImage,
    source: MachineId,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<ImageIngestDestination, PushError> {
    Ok(
        Source::open(client, source, &mut Cancellation::new(cancellation))
            .await?
            .require_complete(image)?
            .destination,
    )
}

/// A Machine whose actual image store was read and whose image server is open.
/// It serves a destination only a variant that store demonstrably holds.
#[derive(Debug)]
pub(super) struct Source {
    machine_id: MachineId,
    destination: ImageIngestDestination,
    store: MachineImages,
}

impl Source {
    /// Read the Machine's store and open its image server. Only the containerd
    /// store reports which variants are present, so any other store cannot serve.
    ///
    /// # Errors
    /// Reports cancellation, a store without variant evidence, and a failed
    /// ingest start.
    pub(super) async fn open(
        client: &mut Client,
        machine_id: MachineId,
        cancellation: &mut Cancellation<'_>,
    ) -> Result<Self, PushError> {
        let target = MachineTarget::from(&machine_id);
        // Docker's reference filter does not match repository@digest; read the
        // whole store and compare identities.
        let store = cancellation
            .race(
                client.call::<op::ListImages>(ListImagesRequest { reference: None }, Some(&target)),
            )
            .await??;
        if !store.containerd_store {
            return Err(PushError::UnsupportedImageStore);
        }
        let opened = cancellation
            .race(client.call::<op::EnsureImageIngest>(EnsureImageIngestRequest {}, Some(&target)))
            .await?
            .map_err(|error| ingest_error(rpc_error(error)))?;
        Ok(Self {
            machine_id,
            destination: opened.destination,
            store,
        })
    }

    /// The Build host must hold every platform the attempt verified; anything
    /// less is a partial peer, not the complete Build.
    ///
    /// # Errors
    /// Names every verified platform this store lacks.
    pub(super) fn require_complete(&self, image: &BuiltImage) -> Result<&Self, PushError> {
        let missing = image
            .platforms
            .iter()
            .filter(|platform| !holds_platform(&self.store, &image.reference, platform))
            .cloned()
            .collect::<Vec<_>>();
        if missing.is_empty() {
            Ok(self)
        } else {
            Err(PushError::BuildIncomplete {
                image: image.reference.clone(),
                machine_id: self.machine_id,
                missing,
            })
        }
    }

    /// The variant of `image` this source holds for `machine`: `platform` when
    /// the caller fixed one, otherwise the one the Machine's architecture runs.
    ///
    /// # Errors
    /// Names the platform the source does not hold.
    pub(super) fn variant<'select>(
        &'select self,
        image: &str,
        machine: &Machine,
        platform: Option<&'select str>,
    ) -> Result<&'select str, PushError> {
        let held = match platform {
            Some(platform) => holds_platform(&self.store, image, platform).then_some(platform),
            None => available_variant(&self.store, image, &machine.runtime.architecture),
        };
        held.ok_or_else(|| PushError::VariantUnavailable {
            image: image.to_owned(),
            machine_id: self.machine_id,
            platform: platform.unwrap_or(&machine.runtime.architecture).to_owned(),
        })
    }

    /// Have `machine` pull the exact content and publish its requested tag,
    /// naming the variant [`Self::variant`] selected for that content.
    ///
    /// # Errors
    /// Reports a variant this source does not hold, cancellation, and a failed pull.
    pub(super) async fn deliver(
        &self,
        client: &mut Client,
        content: ImageContent<'_>,
        machine: &Machine,
        platform: Option<&str>,
        cancellation: &mut Cancellation<'_>,
    ) -> Result<(), PushError> {
        let variant = self.variant(content.exact(), machine, platform)?;
        pull_on_machine(
            client,
            content,
            machine,
            self.destination,
            variant,
            cancellation,
        )
        .await
    }
}

/// The platform a Machine's store demonstrably holds for `image` that
/// `architecture` runs natively, if any.
///
/// `image` names exact content when it carries a digest (`sha256:…` or
/// `repository@sha256:…`): it is matched against the store's image identity,
/// so a tag that moved to another image cannot substitute. Any other reference
/// is matched by tag. Only platforms Docker reports available count, meaning
/// the manifest, configuration and every layer are present locally. A tag, an
/// index descriptor, or a listed-but-absent variant proves nothing: a peer that
/// pulled one platform keeps the whole index while lacking the other variant's data.
pub(crate) fn available_variant<'store>(
    store: &'store MachineImages,
    image: &str,
    architecture: &str,
) -> Option<&'store str> {
    stored(store, image)?
        .platforms
        .iter()
        .map(String::as_str)
        .find(|platform| platform_compatible(platform, architecture))
}

/// Whether the store holds `image` content for exactly `platform`.
fn holds_platform(store: &MachineImages, image: &str, platform: &str) -> bool {
    stored(store, image).is_some_and(|summary| {
        summary
            .platforms
            .iter()
            .any(|stored| same_platform(stored, platform))
    })
}

fn stored<'store>(store: &'store MachineImages, image: &str) -> Option<&'store ImageSummary> {
    if !store.containerd_store {
        return None;
    }
    let digest = image
        .rsplit_once('@')
        .map(|(_, digest)| digest)
        .or_else(|| image.starts_with("sha256:").then_some(image));
    store.images.iter().find(|summary| match digest {
        Some(digest) => summary.id == digest,
        None => summary.repo_tags.iter().any(|tag| {
            tag == image
                || tag
                    .strip_suffix(image)
                    .is_some_and(|prefix| prefix.ends_with('/'))
        }),
    })
}

/// Docker lists ARM64 with or without its only variant.
fn same_platform(left: &str, right: &str) -> bool {
    left == right
        || matches!(
            (left, right),
            ("linux/arm64", "linux/arm64/v8") | ("linux/arm64/v8", "linux/arm64")
        )
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
    use super::*;

    fn store(id: &str, tags: &[&str], platforms: &[&str]) -> MachineImages {
        MachineImages {
            containerd_store: true,
            images: vec![ImageSummary {
                id: id.into(),
                repo_tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
                created: 0,
                size: 0,
                containers: 0,
                platforms: platforms
                    .iter()
                    .map(|platform| (*platform).to_owned())
                    .collect(),
            }],
        }
    }

    #[test]
    fn a_source_serves_only_a_variant_it_demonstrably_holds() {
        let digest = format!("sha256:{}", "1".repeat(64));
        let repository = format!("registry.invalid/api@{digest}");
        // The prototype's partial peer: the full index remains while the
        // unselected platform's manifest, configuration and layers are absent.
        let partial = store(&digest, &["registry.invalid/api:v1"], &["linux/arm64/v8"]);
        assert_eq!(
            available_variant(&partial, &repository, "aarch64"),
            Some("linux/arm64/v8")
        );
        assert_eq!(available_variant(&partial, &repository, "x86_64"), None);
        assert_eq!(available_variant(&partial, &digest, "x86_64"), None);
        assert!(holds_platform(&partial, &digest, "linux/arm64"));
        assert!(!holds_platform(&partial, &digest, "linux/amd64"));

        // A tag race: the requested tag now names another image, but the
        // exact content still identifies itself.
        let moved = store(&digest, &["registry.invalid/api:other"], &["linux/amd64"]);
        assert_eq!(
            available_variant(&moved, &repository, "x86_64"),
            Some("linux/amd64")
        );
        assert_eq!(
            available_variant(&moved, "registry.invalid/api:v1", "x86_64"),
            None
        );

        // A tag alone: no available platform is no deliverable content.
        let tag_only = store("sha256:other", &["docker.io/library/busybox:1.37.0"], &[]);
        assert_eq!(
            available_variant(&tag_only, "busybox:1.37.0", "x86_64"),
            None
        );
        let tagged = store(
            "sha256:other",
            &["docker.io/library/busybox:1.37.0"],
            &["linux/amd64"],
        );
        assert_eq!(
            available_variant(&tagged, "busybox:1.37.0", "x86_64"),
            Some("linux/amd64")
        );
        assert_eq!(available_variant(&tagged, "busybox:1.37.0", ""), None);
        assert_eq!(available_variant(&tagged, "busybox:1.36.0", "x86_64"), None);

        // Only the containerd store reports available variants.
        let mut classic = tagged;
        classic.containerd_store = false;
        assert_eq!(
            available_variant(&classic, "busybox:1.37.0", "x86_64"),
            None
        );
    }

    #[test]
    fn the_build_host_must_hold_every_verified_platform() {
        let digest = format!("sha256:{}", "2".repeat(64));
        let image = BuiltImage {
            reference: digest.clone(),
            tags: vec!["registry.invalid/api:v1".into()],
            platforms: vec!["linux/amd64".into(), "linux/arm64".into()],
            location: "unix:///var/run/docker.sock".into(),
        };
        let source = |store| Source {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            destination: ImageIngestDestination {
                management_address: ployz_core::ManagementAddress("fdcc::7".parse().unwrap()),
                port: ployz_core::UNREGISTRY_PORT,
            },
            store,
        };
        let complete = source(store(&digest, &[], &["linux/amd64", "linux/arm64/v8"]));
        complete.require_complete(&image).unwrap();
        let partial = source(store(
            &digest,
            &["registry.invalid/api:v1"],
            &["linux/amd64"],
        ));
        let error = partial.require_complete(&image).unwrap_err();
        assert!(
            matches!(&error, PushError::BuildIncomplete { missing, .. } if missing == &["linux/arm64"]),
            "{error}"
        );
    }

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
