use ployz_core::deploy::{ContainerMountPath, ServiceVolumeMount, VolumeName};
use serde_yaml::Value;

use super::diagnostics::{ComposeFinding, ComposePath};
use super::model::{ComposeKnownVolumeType, ComposeLongVolume, ComposeVolume, ComposeVolumeType};

pub(super) fn parse_volume_mounts(
    volumes: Option<Vec<ComposeVolume>>,
    service_path: &ComposePath,
) -> Result<Vec<ServiceVolumeMount>, Vec<ComposeFinding>> {
    let Some(volumes) = volumes else {
        return Ok(Vec::new());
    };
    let path = service_path.field("volumes");
    let mut mounts = Vec::new();
    let mut findings = Vec::new();
    for (index, volume) in volumes.into_iter().enumerate() {
        let entry_path = path.index(index);
        let parsed = match volume {
            ComposeVolume::Short(value) => {
                parse_short_volume_mount(&value, &entry_path).map_err(|finding| vec![finding])
            }
            ComposeVolume::Long(volume) => parse_long_volume_mount(*volume, &entry_path),
        };
        match parsed {
            Ok(mount) => mounts.push(mount),
            Err(mut entry_findings) => findings.append(&mut entry_findings),
        }
    }
    if findings.is_empty() {
        Ok(mounts)
    } else {
        Err(findings)
    }
}

fn parse_short_volume_mount(
    value: &str,
    path: &ComposePath,
) -> Result<ServiceVolumeMount, ComposeFinding> {
    let Some((source, target)) = value.split_once(':') else {
        return Err(ComposeFinding::invalid(
            path.clone(),
            "anonymous volumes are not supported; name the volume, for example data:/data",
        ));
    };
    if target.contains(':') {
        return Err(ComposeFinding::invalid(
            path.clone(),
            "read-only volume mounts and mount options are not supported yet; remove the third component",
        ));
    }
    named_volume_mount(source, target, path, path)
}

fn parse_long_volume_mount(
    volume: ComposeLongVolume,
    path: &ComposePath,
) -> Result<ServiceVolumeMount, Vec<ComposeFinding>> {
    let ComposeLongVolume {
        mount_type,
        source,
        target,
        read_only,
        volume,
        bind,
        tmpfs,
        unrecognized,
    } = volume;
    match mount_type {
        Some(ComposeVolumeType::Known(ComposeKnownVolumeType::Bind)) => {
            return Err(vec![bind_mount_finding(path.field("type"))]);
        }
        Some(ComposeVolumeType::Known(ComposeKnownVolumeType::Tmpfs)) => {
            return Err(vec![ComposeFinding::invalid(
                path.field("type"),
                "tmpfs mounts are not supported; use a named volume",
            )]);
        }
        Some(ComposeVolumeType::Known(ComposeKnownVolumeType::Volume)) | None => {}
        Some(ComposeVolumeType::Other(other)) => {
            return Err(vec![ComposeFinding::invalid(
                path.field("type"),
                format!(
                    "volume mount type {other:?} is not supported; use type: volume with a named source"
                ),
            )]);
        }
    }

    let mut findings = Vec::new();
    if read_only == Some(true) {
        findings.push(ComposeFinding::invalid(
            path.field("read_only"),
            "read-only volume mounts are not supported yet; remove read_only or set it to false",
        ));
    }
    for (field, value, message) in [
        (
            "volume",
            volume,
            "volume mount options are not supported yet; remove volume",
        ),
        (
            "bind",
            bind,
            "bind mount options are not supported; use a plain named volume mount",
        ),
        (
            "tmpfs",
            tmpfs,
            "tmpfs mount options are not supported; use a named volume",
        ),
    ] {
        if value.is_some() {
            findings.push(ComposeFinding::invalid(path.field(field), message));
        }
    }
    for (key, _value) in unrecognized {
        findings.push(ComposeFinding::invalid(
            path.field(&key),
            format!("volume option {key:?} is not supported; remove it"),
        ));
    }

    if source.is_none() {
        findings.push(ComposeFinding::invalid(
            path.field("source"),
            "long-syntax volume mount requires source; name the volume",
        ));
    }
    if target.is_none() {
        findings.push(ComposeFinding::invalid(
            path.field("target"),
            "long-syntax volume mount requires target; set an absolute container path",
        ));
    }
    let Some((source, target)) = source.zip(target) else {
        return Err(findings);
    };
    match named_volume_mount(
        &source,
        &target,
        &path.field("source"),
        &path.field("target"),
    ) {
        Ok(mount) if findings.is_empty() => Ok(mount),
        Ok(_mount) => Err(findings),
        Err(finding) => {
            findings.push(finding);
            Err(findings)
        }
    }
}

fn named_volume_mount(
    source: &str,
    target: &str,
    source_path: &ComposePath,
    target_path: &ComposePath,
) -> Result<ServiceVolumeMount, ComposeFinding> {
    if source.starts_with('/') || source.starts_with('.') || source.starts_with('~') {
        return Err(bind_mount_finding(source_path.clone()));
    }
    let volume_name = VolumeName::try_new(source)
        .map_err(|error| ComposeFinding::invalid(source_path.clone(), error))?;
    let target = ContainerMountPath::try_new(target)
        .map_err(|error| ComposeFinding::invalid(target_path.clone(), error))?;
    Ok(ServiceVolumeMount {
        volume_name,
        target,
    })
}

fn bind_mount_finding(path: ComposePath) -> ComposeFinding {
    ComposeFinding::invalid(
        path,
        "bind mounts are not supported; use a named volume such as data:/data",
    )
}

pub(super) fn validate_top_level_volumes(
    volumes: Option<Value>,
    findings: &mut Vec<ComposeFinding>,
) {
    let Some(volumes) = volumes else {
        return;
    };
    let volumes_path = ComposePath::root().field("volumes");
    let Value::Mapping(volumes) = volumes else {
        if !matches!(volumes, Value::Null) {
            findings.push(ComposeFinding::invalid(
                volumes_path,
                "volumes must be a mapping of volume names to declarations",
            ));
        }
        return;
    };
    for (name, declaration) in volumes {
        let Some(name) = name.as_str() else {
            findings.push(ComposeFinding::invalid(
                volumes_path.clone(),
                "volume names must be strings",
            ));
            continue;
        };
        validate_top_level_volume_declaration(declaration, &volumes_path.field(name), findings);
    }
}

fn validate_top_level_volume_declaration(
    declaration: Value,
    path: &ComposePath,
    findings: &mut Vec<ComposeFinding>,
) {
    let Value::Mapping(options) = declaration else {
        if !matches!(declaration, Value::Null) {
            findings.push(ComposeFinding::invalid(
                path.clone(),
                "volume declaration must be null or a mapping",
            ));
        }
        return;
    };
    for (key, value) in options {
        let Some(key) = key.as_str() else {
            findings.push(ComposeFinding::invalid(
                path.clone(),
                "volume declaration keys must be strings",
            ));
            continue;
        };
        match key {
            "external" if value == Value::Bool(false) => {}
            "external" => findings.push(ComposeFinding::invalid(
                path.field(key),
                "external volumes are not supported; remove external: true so Ployz creates the volume as pinned intent",
            )),
            "driver" => findings.push(ComposeFinding::invalid(
                path.field(key),
                "custom volume drivers are not supported; remove driver",
            )),
            "driver_opts" => findings.push(ComposeFinding::invalid(
                path.field(key),
                "volume driver options are not supported; remove driver_opts",
            )),
            "name" => findings.push(ComposeFinding::invalid(
                path.field(key),
                "volume name overrides are not supported; use the top-level volume key",
            )),
            "labels" => findings.push(ComposeFinding::invalid(
                path.field(key),
                "volume labels are not supported; remove labels",
            )),
            other => findings.push(ComposeFinding::invalid(
                path.field(other),
                format!("volume declaration option {other:?} is not supported; remove it"),
            )),
        }
    }
}
