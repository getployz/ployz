use std::collections::BTreeMap;

use ployz_core::{
    ContainerPath, DockerVolumeName, MachinePath, ServiceMount, ServiceVolume,
    ServiceVolumeReference, VolumeDriver, VolumeSource,
};
use sha2::{Digest, Sha256};

use super::{
    convert::{bytes_u64, integer, invalid, is_external},
    model::{ComposeError, RawProject, RawService, RawServiceVolume},
};

pub(super) fn volumes(
    service: &RawService,
    root: &RawProject,
) -> Result<(Vec<ServiceVolume>, Vec<ServiceMount>), ComposeError> {
    if let Some(source) = relative_bind_source(service) {
        return Err(invalid(format!("bind mount source '{source}' is relative")));
    }
    let mut specs = BTreeMap::new();
    let mut mounts = Vec::new();
    for item in &service.volumes {
        let (kind, source, target, read_only, bind, volume, tmpfs) = match item {
            RawServiceVolume::Short(value) => {
                let parts = value.split(':').collect::<Vec<_>>();
                let Some((source, rest)) = parts.split_first() else {
                    return Err(invalid("volume short syntax requires source and target"));
                };
                let Some(target) = rest.first() else {
                    return Err(invalid("volume short syntax requires source and target"));
                };
                (
                    if source.starts_with('/') {
                        "bind"
                    } else {
                        "volume"
                    },
                    Some((*source).to_owned()),
                    (*target).to_owned(),
                    parts.get(2) == Some(&"ro"),
                    None,
                    None,
                    None,
                )
            }
            RawServiceVolume::Long(volume) => (
                volume.kind.as_str(),
                volume.source.clone(),
                volume.target.clone(),
                volume.read_only,
                volume.bind.as_ref(),
                volume.volume.as_ref(),
                volume.tmpfs.as_ref(),
            ),
        };
        let reference = match kind {
            "bind" => format!("bind-{}", sha256(&target)),
            "tmpfs" => format!("tmpfs-{}", sha256(&target)),
            "volume" => source
                .clone()
                .ok_or_else(|| invalid("named volume requires source"))?,
            kind => return Err(invalid(format!("unsupported volume type: '{kind}'"))),
        };
        let volume_source = match kind {
            "bind" => VolumeSource::Bind {
                machine_path: MachinePath::parse(
                    source.ok_or_else(|| invalid("bind mount requires source"))?,
                )
                .map_err(invalid)?,
                create_machine_path: bind.is_some_and(|bind| bind.create_host_path),
                propagation: bind
                    .and_then(|bind| bind.propagation.as_deref())
                    .map(str::parse)
                    .transpose()
                    .map_err(invalid)?,
                recursive: bind
                    .and_then(|bind| bind.recursive.as_deref())
                    .map(str::parse)
                    .transpose()
                    .map_err(invalid)?,
            },
            "tmpfs" => VolumeSource::Tmpfs {
                size_bytes: tmpfs
                    .and_then(|tmpfs| tmpfs.size.as_ref())
                    .map(|size| {
                        bytes_u64(size).ok_or_else(|| invalid("tmpfs.size must be a byte size"))
                    })
                    .transpose()?,
                mode: tmpfs
                    .and_then(|tmpfs| tmpfs.mode.as_ref())
                    .map(|mode| {
                        integer(mode)
                            .and_then(|mode| u32::try_from(mode).ok())
                            .ok_or_else(|| invalid("tmpfs.mode must be a non-negative integer"))
                    })
                    .transpose()?,
                options: Vec::new(),
            },
            "volume" => {
                let key = source.ok_or_else(|| invalid("named volume requires source"))?;
                let declared = root.volumes.get(&key).ok_or_else(|| {
                    invalid(format!("volume '{key}' not found in project volumes"))
                })?;
                let external = is_external(&declared.external);
                let docker_name = if external {
                    declared
                        .name
                        .as_deref()
                        .filter(|name| !name.is_empty())
                        .unwrap_or(key.as_str())
                        .to_owned()
                } else {
                    key.clone()
                };
                VolumeSource::Named {
                    name: DockerVolumeName::parse(docker_name).map_err(invalid)?,
                    external,
                    driver: (!external
                        && (declared.driver.is_some() || !declared.driver_opts.is_empty()))
                    .then(|| VolumeDriver {
                        name: declared.driver.clone().unwrap_or_default(),
                        options: declared.driver_opts.clone(),
                    }),
                    labels: if external {
                        BTreeMap::new()
                    } else {
                        declared.labels.clone()
                    },
                    no_copy: volume.is_some_and(|volume| volume.nocopy),
                    subpath: volume.and_then(|volume| volume.subpath.clone()),
                }
            }
            _ => unreachable!("validated volume kind"),
        };
        let reference = ServiceVolumeReference::parse(reference).map_err(invalid)?;
        if let Some(existing) = specs.get(&reference) {
            if existing != &volume_source {
                return Err(invalid(format!(
                    "volume '{reference}' is used multiple times with different options"
                )));
            }
        } else {
            specs.insert(reference.clone(), volume_source);
        }
        mounts.push(ServiceMount {
            volume: reference,
            target: ContainerPath::parse(target).map_err(invalid)?,
            read_only,
        });
    }
    Ok((
        specs
            .into_iter()
            .map(|(reference, source)| ServiceVolume { reference, source })
            .collect(),
        mounts,
    ))
}

pub(super) fn relative_bind_source(service: &RawService) -> Option<&str> {
    service.volumes.iter().find_map(|volume| {
        let source = match volume {
            RawServiceVolume::Short(value) => value.split(':').next(),
            RawServiceVolume::Long(volume) if volume.kind == "bind" => volume.source.as_deref(),
            RawServiceVolume::Long(_) => None,
        }?;
        (source.starts_with('.') || source.starts_with('~')).then_some(source)
    })
}

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
