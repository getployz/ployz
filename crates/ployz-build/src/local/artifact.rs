use ployz_build_api::{BuildCommand, BuildCommandOutput};
use ployz_model::{
    BuildLocalRequest, BuildLocation, ImageArtifact, ImageArtifactProvenance,
    ImageAvailabilityRecord, ImageDigest, ImagePresence, ImageRef,
};
use ployz_runtime_api::{RuntimeImage, RuntimeImageError};
use ployz_time::now_unix_secs;

#[must_use]
pub fn normalize_build_image_name(reference: &str) -> String {
    if reference.starts_with("sha256:") || image_reference_has_tag(reference) {
        reference.into()
    } else {
        format!("{reference}:latest")
    }
}

fn image_reference_has_tag(reference: &str) -> bool {
    let last_slash = reference.rfind('/');
    reference.rfind(':').is_some_and(|index| {
        index + 1 < reference.len() && last_slash.is_none_or(|slash| index > slash)
    })
}

pub fn build_image_artifact(
    request: &BuildLocalRequest,
    image: &RuntimeImage,
) -> Result<ImageArtifact, String> {
    let digest = runtime_image_identity(&request.image_name, image)?;
    let now = now_unix_secs();
    Ok(ImageArtifact {
        image: image_ref_from_tag(&request.image_name, digest),
        platform: request.platform.clone().or_else(|| image.platform.clone()),
        provenance: ImageArtifactProvenance::Build {
            method: request.method,
            location: BuildLocation::Local,
            source_digest: None,
        },
        created_at: now,
    })
}

fn runtime_image_identity(reference: &str, image: &RuntimeImage) -> Result<ImageDigest, String> {
    let Some(id) = image.id.as_deref() else {
        return Err(RuntimeImageError::MissingDigest {
            reference: reference.into(),
        }
        .to_string());
    };
    ImageDigest::try_new(id).map_err(|_| {
        RuntimeImageError::MissingDigest {
            reference: reference.into(),
        }
        .to_string()
    })
}

#[must_use]
pub fn present_build_availability(
    machine_id: &ployz_model::MachineId,
    artifact: ImageArtifact,
    operation_id: &str,
) -> ImageAvailabilityRecord {
    let now = now_unix_secs();
    ImageAvailabilityRecord {
        machine_id: machine_id.clone(),
        digest: artifact.image.digest().clone(),
        presence: ImagePresence::Present {
            artifact,
            recorded_at: now,
            source_operation_id: Some(operation_id.into()),
        },
        updated_at: now,
    }
}

fn image_ref_from_tag(reference: &str, digest: ImageDigest) -> ImageRef {
    if reference.starts_with("sha256:") {
        return ImageRef::digest_only(digest);
    }

    let (repository, tag) = {
        let last_slash = reference.rfind('/');
        match reference
            .rfind(':')
            .filter(|index| last_slash.is_none_or(|slash| *index > slash))
        {
            Some(index) if index + 1 < reference.len() => {
                let (repository, tag) = reference.split_at(index);
                (repository.to_string(), Some(tag[1..].to_string()))
            }
            _ => (reference.to_string(), None),
        }
    };
    ImageRef::repository_digest(repository, tag, digest)
}

#[must_use]
pub fn render_build_result(
    operation_id: &str,
    record: &ImageAvailabilityRecord,
    output: BuildCommandOutput,
    command: &BuildCommand,
) -> String {
    let mut message = format!(
        "{}  {}  {}  present",
        operation_id,
        record.machine_id,
        record.digest.as_str()
    );
    if !output.stdout.trim().is_empty() {
        message.push('\n');
        message.push_str(&command.redact_captured_output(output.stdout.trim()));
    }
    message
}
