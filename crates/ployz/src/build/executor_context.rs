//! Private, identity-keyed connection material for external Build Executors.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use ployz_core::build::BuildExecutorIdentity;
use ployz_core::ids::{BuildExecutorId, BuildPoolId, SubjectToken};
use ployz_core::nats_config::{
    BuildExecutorCredentialExpiresAt, MintedNatsUser, NatsServerConfigError, NatsUserSeed,
};
use ployz_nats::connect::{NatsClientUrl, NatsClientUrlError};
use serde::{Deserialize, Serialize};

const CONTEXT_FILE: &str = "context.json";
const CA_FILE: &str = "ca.pem";
const SEED_FILE: &str = "nkey.seed";

#[cfg(unix)]
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExecutorContext {
    pub organization_id: SubjectToken,
    pub pool_id: BuildPoolId,
    pub executor_id: BuildExecutorId,
    pub nats_url: NatsClientUrl,
    pub nats_ca_file: PathBuf,
    pub nats_seed: NatsUserSeed,
    pub credential_expires_at: BuildExecutorCredentialExpiresAt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExecutorIdentityPaths {
    identity: BuildExecutorIdentity,
    pub directory: PathBuf,
    pub context: PathBuf,
    pub seed: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutorContextFile {
    organization_id: SubjectToken,
    pool_id: BuildPoolId,
    executor_id: BuildExecutorId,
    runtime_nats_url: String,
    nats_ca_file: PathBuf,
    credential_expires_at: BuildExecutorCredentialExpiresAt,
}

impl ExecutorIdentityPaths {
    #[must_use]
    pub fn new(root: &Path, identity: BuildExecutorIdentity) -> Self {
        let directory = root
            .join(identity.pool_id.as_str())
            .join(identity.executor_id.as_str());
        Self {
            identity,
            context: directory.join(CONTEXT_FILE),
            seed: directory.join(SEED_FILE),
            directory,
        }
    }

    #[must_use]
    pub const fn identity(&self) -> &BuildExecutorIdentity {
        &self.identity
    }
}

#[must_use]
fn default_executor_context_root() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(config_home.join("ployz").join("build-executors"))
}

pub(super) fn resolve_executor_context_root(configured: Option<&Path>) -> Option<PathBuf> {
    configured
        .map(Path::to_path_buf)
        .or_else(default_executor_context_root)
}

pub(super) fn load_executor_context(
    paths: &ExecutorIdentityPaths,
) -> Result<ExecutorContext, ExecutorContextError> {
    let raw = fs::read_to_string(&paths.context).map_err(|error| ExecutorContextError::Read {
        path: paths.context.clone(),
        message: error.to_string(),
    })?;
    let file: ExecutorContextFile =
        serde_json::from_str(&raw).map_err(|error| ExecutorContextError::Parse {
            path: paths.context.clone(),
            message: error.to_string(),
        })?;
    let ExecutorContextFile {
        organization_id,
        pool_id: stored_pool_id,
        executor_id: stored_executor_id,
        runtime_nats_url,
        nats_ca_file,
        credential_expires_at,
    } = file;
    if stored_pool_id != paths.identity.pool_id || stored_executor_id != paths.identity.executor_id
    {
        return Err(ExecutorContextError::IdentityMismatch);
    }
    if !is_private_ca_reference(&nats_ca_file) {
        return Err(ExecutorContextError::InvalidMaterialReference);
    }
    let raw_seed = fs::read_to_string(&paths.seed).map_err(|error| ExecutorContextError::Read {
        path: paths.seed.clone(),
        message: error.to_string(),
    })?;
    let nats_seed = NatsUserSeed::try_new(raw_seed).map_err(ExecutorContextError::Nkey)?;
    Ok(ExecutorContext {
        organization_id,
        pool_id: stored_pool_id,
        executor_id: stored_executor_id,
        nats_url: NatsClientUrl::try_new(runtime_nats_url)
            .map_err(ExecutorContextError::InvalidNatsUrl)?,
        nats_ca_file: paths.directory.join(nats_ca_file),
        nats_seed,
        credential_expires_at,
    })
}

pub(super) fn load_or_create_identity(
    paths: &ExecutorIdentityPaths,
) -> Result<MintedNatsUser, ExecutorContextError> {
    prepare_identity_hierarchy(paths)?;
    match fs::read_to_string(&paths.seed) {
        Ok(raw) => load_identity_seed(&paths.seed, raw),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let minted = MintedNatsUser::generate().map_err(ExecutorContextError::Nkey)?;
            match write_new_private_file(&paths.seed, minted.seed.secret()) {
                Ok(()) => Ok(minted),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let raw = fs::read_to_string(&paths.seed).map_err(|source| {
                        ExecutorContextError::Read {
                            path: paths.seed.clone(),
                            message: source.to_string(),
                        }
                    })?;
                    load_identity_seed(&paths.seed, raw)
                }
                Err(error) => Err(ExecutorContextError::Write {
                    path: paths.seed.clone(),
                    message: error.to_string(),
                }),
            }
        }
        Err(error) => Err(ExecutorContextError::Read {
            path: paths.seed.clone(),
            message: error.to_string(),
        }),
    }
}

pub(super) struct ExecutorContextPublication<'a> {
    pub organization_id: &'a SubjectToken,
    pub nats_url: NatsClientUrl,
    pub ca_pem: &'a str,
    pub credential_expires_at: BuildExecutorCredentialExpiresAt,
}

pub(super) fn publish_executor_context(
    paths: &ExecutorIdentityPaths,
    publication: ExecutorContextPublication<'_>,
) -> Result<ExecutorContext, ExecutorContextError> {
    let ExecutorContextPublication {
        organization_id,
        nats_url,
        ca_pem,
        credential_expires_at,
    } = publication;
    prepare_identity_hierarchy(paths)?;
    let (ca_directory, ca_relative) = create_ca_generation(paths, ca_pem)?;
    let identity = paths.identity();
    let file = ExecutorContextFile {
        organization_id: organization_id.clone(),
        pool_id: identity.pool_id.clone(),
        executor_id: identity.executor_id.clone(),
        runtime_nats_url: nats_url.as_str().to_owned(),
        nats_ca_file: ca_relative,
        credential_expires_at,
    };
    let mut payload =
        serde_json::to_vec_pretty(&file).map_err(|error| ExecutorContextError::Write {
            path: paths.context.clone(),
            message: error.to_string(),
        })?;
    payload.push(b'\n');
    if let Err(error) = atomic_replace_private_file(&paths.context, &payload) {
        let _ = fs::remove_dir_all(ca_directory);
        return Err(error);
    }
    load_executor_context(paths)
}

fn load_identity_seed(path: &Path, raw: String) -> Result<MintedNatsUser, ExecutorContextError> {
    let seed = NatsUserSeed::try_new(raw).map_err(ExecutorContextError::Nkey)?;
    MintedNatsUser::from_seed(seed).map_err(|source| ExecutorContextError::InvalidSeed {
        path: path.to_owned(),
        source,
    })
}

fn write_new_private_file(path: &Path, contents: &str) -> Result<(), std::io::Error> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private file has no parent directory",
        ));
    };
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    temporary.as_file().set_permissions({
        use std::os::unix::fs::PermissionsExt as _;
        fs::Permissions::from_mode(PRIVATE_FILE_MODE)
    })?;
    temporary.write_all(contents.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    sync_directory(parent)
}

fn create_ca_generation(
    paths: &ExecutorIdentityPaths,
    ca_pem: &str,
) -> Result<(PathBuf, PathBuf), ExecutorContextError> {
    let materials = paths.directory.join("materials");
    prepare_private_directory(&materials)?;
    let directory = tempfile::Builder::new()
        .prefix("material-")
        .tempdir_in(&materials)
        .map_err(|error| ExecutorContextError::Write {
            path: materials,
            message: error.to_string(),
        })?
        .keep();
    prepare_private_directory(&directory)?;
    let ca_path = directory.join(CA_FILE);
    write_new_private_file(&ca_path, ca_pem).map_err(|error| ExecutorContextError::Write {
        path: ca_path,
        message: error.to_string(),
    })?;
    let Some(generation_name) = directory.file_name() else {
        return Err(ExecutorContextError::Write {
            path: directory,
            message: "CA generation has no directory name".to_owned(),
        });
    };
    let relative = PathBuf::from("materials")
        .join(generation_name)
        .join(CA_FILE);
    Ok((directory, relative))
}

fn is_private_ca_reference(path: &Path) -> bool {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    matches!(components.as_slice(), [materials, generation, ca]
        if materials == "materials" && generation.starts_with("material-") && ca == CA_FILE)
}

fn atomic_replace_private_file(path: &Path, contents: &[u8]) -> Result<(), ExecutorContextError> {
    let Some(parent) = path.parent() else {
        return Err(ExecutorContextError::Write {
            path: path.to_owned(),
            message: "file has no parent directory".to_owned(),
        });
    };
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| ExecutorContextError::Write {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions({
            use std::os::unix::fs::PermissionsExt as _;
            fs::Permissions::from_mode(PRIVATE_FILE_MODE)
        })
        .map_err(|error| ExecutorContextError::Write {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    temporary
        .write_all(contents)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| ExecutorContextError::Write {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    temporary
        .persist(path)
        .map_err(|error| ExecutorContextError::Write {
            path: path.to_owned(),
            message: error.error.to_string(),
        })?;
    sync_directory(parent).map_err(|error| ExecutorContextError::Write {
        path: path.to_owned(),
        message: error.to_string(),
    })
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

fn prepare_private_directory(path: &Path) -> Result<(), ExecutorContextError> {
    fs::create_dir_all(path).map_err(|error| ExecutorContextError::Write {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    #[cfg(unix)]
    fs::set_permissions(path, {
        use std::os::unix::fs::PermissionsExt as _;
        fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)
    })
    .map_err(|error| ExecutorContextError::Write {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    Ok(())
}

fn prepare_identity_hierarchy(paths: &ExecutorIdentityPaths) -> Result<(), ExecutorContextError> {
    let Some(pool_directory) = paths.directory.parent() else {
        return Err(ExecutorContextError::Write {
            path: paths.directory.clone(),
            message: "executor identity has no pool directory".to_owned(),
        });
    };
    let Some(root) = pool_directory.parent() else {
        return Err(ExecutorContextError::Write {
            path: paths.directory.clone(),
            message: "executor identity has no context root".to_owned(),
        });
    };
    for directory in [root, pool_directory, paths.directory.as_path()] {
        prepare_private_directory(directory)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum ExecutorContextError {
    #[error("executor context file {} is unreadable: {message}", path.display())]
    Read { path: PathBuf, message: String },
    #[error("executor context file {} is invalid: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("could not write executor context material at {}: {message}", path.display())]
    Write { path: PathBuf, message: String },
    #[error("executor context identity does not match the requested executor")]
    IdentityMismatch,
    #[error("executor context must reference its own generated CA")]
    InvalidMaterialReference,
    #[error("executor context has an invalid NATS URL: {0}")]
    InvalidNatsUrl(NatsClientUrlError),
    #[error("executor NKey material is invalid: {0}")]
    Nkey(NatsServerConfigError),
    #[error("executor seed file {} is invalid: {source}", path.display())]
    InvalidSeed {
        path: PathBuf,
        source: NatsServerConfigError,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::thread;

    use ployz_core::build::BuildExecutorIdentity;
    use ployz_core::ids::{BuildExecutorId, BuildPoolId};
    use ployz_nats::connect::NatsClientUrl;

    use super::{
        ExecutorContextPublication, ExecutorIdentityPaths, load_executor_context,
        load_or_create_identity, publish_executor_context,
    };

    #[test]
    fn context_swap_never_pairs_old_identity_with_new_ca() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = Arc::new(temporary.path().join("contexts"));
        let pool_id = BuildPoolId::try_new("homelab").expect("pool id");
        let executor_id = BuildExecutorId::try_new("builder-1").expect("executor id");
        let paths = ExecutorIdentityPaths::new(
            &root,
            BuildExecutorIdentity {
                pool_id: pool_id.clone(),
                executor_id: executor_id.clone(),
            },
        );
        load_or_create_identity(&paths).expect("identity exists");
        publish(&paths, "one");

        let reader_root = Arc::clone(&root);
        let reader_pool = pool_id.clone();
        let reader_executor = executor_id.clone();
        let reader = thread::spawn(move || {
            for _ in 0..100 {
                let paths = ExecutorIdentityPaths::new(
                    &reader_root,
                    BuildExecutorIdentity {
                        pool_id: reader_pool.clone(),
                        executor_id: reader_executor.clone(),
                    },
                );
                let context = load_executor_context(&paths).expect("published context loads");
                let ca = fs::read_to_string(&context.nats_ca_file).expect("published CA reads");
                let expected = context
                    .nats_url
                    .as_str()
                    .strip_prefix("tls://")
                    .and_then(|value| value.strip_suffix(".example:4222"))
                    .expect("test URL shape");
                assert_eq!(ca, expected);
            }
        });
        for index in 0..50 {
            let generation = if index % 2 == 0 { "two" } else { "one" };
            publish(&paths, generation);
        }
        reader.join().expect("reader exits");
    }

    #[test]
    fn context_json_omits_fixed_seed_path_and_loads_validated_seed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let identity = BuildExecutorIdentity {
            pool_id: BuildPoolId::try_new("homelab").expect("pool id"),
            executor_id: BuildExecutorId::try_new("builder-1").expect("executor id"),
        };
        let paths = ExecutorIdentityPaths::new(temporary.path(), identity);
        let minted = load_or_create_identity(&paths).expect("identity exists");
        publish(&paths, "one");

        let json = fs::read_to_string(&paths.context).expect("context reads");
        assert!(!json.contains("nats_seed_file"));
        assert!(json.contains("\"credential_expires_at\": \"1\""));
        let context = load_executor_context(&paths).expect("context loads");
        assert_eq!(context.nats_seed.secret(), minted.seed.secret());
    }

    fn publish(paths: &super::ExecutorIdentityPaths, generation: &str) {
        publish_executor_context(
            paths,
            ExecutorContextPublication {
                organization_id: &ployz_core::ids::SubjectToken::try_new("org-1")
                    .expect("organization id"),
                nats_url: NatsClientUrl::try_new(format!("tls://{generation}.example:4222"))
                    .expect("NATS URL"),
                ca_pem: generation,
                credential_expires_at:
                    ployz_core::nats_config::BuildExecutorCredentialExpiresAt::first(),
            },
        )
        .expect("context publishes");
    }
}
