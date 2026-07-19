//! Private, identity-keyed connection material for external Build Executors.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use ployz_core::ids::{BuildExecutorId, BuildPoolId, SubjectToken, SubjectTokenError};
use ployz_core::nats_config::{MintedNatsUser, NatsServerConfigError, NatsUserSeed};
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
pub struct ExecutorContext {
    pub organization_id: String,
    pub pool_id: BuildPoolId,
    pub executor_id: BuildExecutorId,
    pub nats_url: NatsClientUrl,
    pub nats_ca_file: PathBuf,
    pub nats_seed_file: PathBuf,
    pub credential_expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorConnectionInputs {
    pub organization_id: String,
    pub pool_id: BuildPoolId,
    pub executor_id: BuildExecutorId,
    pub nats_url: NatsClientUrl,
    pub nats_ca_file: PathBuf,
    pub nats_seed_file: PathBuf,
    pub credential_expires_at: u64,
}

impl From<ExecutorContext> for ExecutorConnectionInputs {
    fn from(context: ExecutorContext) -> Self {
        let ExecutorContext {
            organization_id,
            pool_id,
            executor_id,
            nats_url,
            nats_ca_file,
            nats_seed_file,
            credential_expires_at,
        } = context;
        Self {
            organization_id,
            pool_id,
            executor_id,
            nats_url,
            nats_ca_file,
            nats_seed_file,
            credential_expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutorIdentityPaths {
    pub directory: PathBuf,
    pub context: PathBuf,
    pub seed: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutorContextFile {
    organization_id: String,
    pool_id: String,
    executor_id: String,
    runtime_nats_url: String,
    nats_ca_file: PathBuf,
    nats_seed_file: PathBuf,
    credential_expires_at: String,
}

#[must_use]
pub fn default_executor_context_root() -> Option<PathBuf> {
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

pub fn load_executor_connection(
    root: &Path,
    pool_id: &BuildPoolId,
    executor_id: &BuildExecutorId,
) -> Result<ExecutorConnectionInputs, ExecutorContextError> {
    load_executor_context(root, pool_id, executor_id).map(Into::into)
}

pub(crate) fn identity_paths(
    root: &Path,
    pool_id: &BuildPoolId,
    executor_id: &BuildExecutorId,
) -> ExecutorIdentityPaths {
    let directory = root.join(pool_id.as_str()).join(executor_id.as_str());
    ExecutorIdentityPaths {
        context: directory.join(CONTEXT_FILE),
        seed: directory.join(SEED_FILE),
        directory,
    }
}

pub(crate) fn load_or_create_identity(
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

pub(crate) struct ExecutorContextPublication<'a> {
    pub organization_id: &'a str,
    pub pool_id: &'a BuildPoolId,
    pub executor_id: &'a BuildExecutorId,
    pub nats_url: NatsClientUrl,
    pub ca_pem: &'a str,
    pub credential_expires_at: u64,
}

pub(crate) fn publish_executor_context(
    paths: &ExecutorIdentityPaths,
    publication: ExecutorContextPublication<'_>,
) -> Result<ExecutorContext, ExecutorContextError> {
    let ExecutorContextPublication {
        organization_id,
        pool_id,
        executor_id,
        nats_url,
        ca_pem,
        credential_expires_at,
    } = publication;
    if SubjectToken::try_new(organization_id).is_err() {
        return Err(ExecutorContextError::InvalidOrganization);
    }
    if credential_expires_at == 0 {
        return Err(ExecutorContextError::InvalidExpiry);
    }
    prepare_identity_hierarchy(paths)?;
    let (ca_directory, ca_relative) = create_ca_generation(paths, ca_pem)?;
    let file = ExecutorContextFile {
        organization_id: organization_id.to_owned(),
        pool_id: pool_id.as_str().to_owned(),
        executor_id: executor_id.as_str().to_owned(),
        runtime_nats_url: nats_url.as_str().to_owned(),
        nats_ca_file: ca_relative,
        nats_seed_file: PathBuf::from(SEED_FILE),
        credential_expires_at: credential_expires_at.to_string(),
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
    let Some(pool_directory) = paths.directory.parent() else {
        return Err(ExecutorContextError::Write {
            path: paths.context.clone(),
            message: "executor context path has no pool directory".to_owned(),
        });
    };
    let Some(root) = pool_directory.parent() else {
        return Err(ExecutorContextError::Write {
            path: paths.context.clone(),
            message: "executor context path has no identity root".to_owned(),
        });
    };
    load_executor_context(root, pool_id, executor_id)
}

fn load_executor_context(
    root: &Path,
    expected_pool_id: &BuildPoolId,
    expected_executor_id: &BuildExecutorId,
) -> Result<ExecutorContext, ExecutorContextError> {
    let paths = identity_paths(root, expected_pool_id, expected_executor_id);
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
        pool_id,
        executor_id,
        runtime_nats_url,
        nats_ca_file,
        nats_seed_file,
        credential_expires_at,
    } = file;
    if SubjectToken::try_new(organization_id.clone()).is_err() {
        return Err(ExecutorContextError::InvalidOrganization);
    }
    let pool_id = BuildPoolId::try_new(pool_id).map_err(ExecutorContextError::InvalidIdentity)?;
    let executor_id =
        BuildExecutorId::try_new(executor_id).map_err(ExecutorContextError::InvalidIdentity)?;
    if pool_id != *expected_pool_id || executor_id != *expected_executor_id {
        return Err(ExecutorContextError::IdentityMismatch);
    }
    if !is_private_ca_reference(&nats_ca_file) || nats_seed_file != Path::new(SEED_FILE) {
        return Err(ExecutorContextError::InvalidMaterialReference);
    }
    let credential_expires_at = credential_expires_at
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ExecutorContextError::InvalidExpiry)?;
    let seed = fs::read_to_string(&paths.seed).map_err(|error| ExecutorContextError::Read {
        path: paths.seed.clone(),
        message: error.to_string(),
    })?;
    NatsUserSeed::try_new(seed).map_err(ExecutorContextError::Nkey)?;
    Ok(ExecutorContext {
        organization_id,
        pool_id,
        executor_id,
        nats_url: NatsClientUrl::try_new(runtime_nats_url)
            .map_err(ExecutorContextError::InvalidNatsUrl)?,
        nats_ca_file: paths.directory.join(nats_ca_file),
        nats_seed_file: paths.seed,
        credential_expires_at,
    })
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
pub enum ExecutorContextError {
    #[error("executor context file {} is unreadable: {message}", path.display())]
    Read { path: PathBuf, message: String },
    #[error("executor context file {} is invalid: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("could not write executor context material at {}: {message}", path.display())]
    Write { path: PathBuf, message: String },
    #[error("executor context has an invalid identity: {0}")]
    InvalidIdentity(SubjectTokenError),
    #[error("executor context identity does not match the requested executor")]
    IdentityMismatch,
    #[error("executor context organization id is invalid")]
    InvalidOrganization,
    #[error("executor context must reference its own generated CA and nkey.seed")]
    InvalidMaterialReference,
    #[error("executor context credential expiry must be a positive decimal string")]
    InvalidExpiry,
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

    use ployz_core::ids::{BuildExecutorId, BuildPoolId};
    use ployz_nats::connect::NatsClientUrl;

    use super::{
        ExecutorContextPublication, identity_paths, load_executor_connection,
        load_or_create_identity, publish_executor_context,
    };

    #[test]
    fn context_swap_never_pairs_old_identity_with_new_ca() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = Arc::new(temporary.path().join("contexts"));
        let pool_id = BuildPoolId::try_new("homelab").expect("pool id");
        let executor_id = BuildExecutorId::try_new("builder-1").expect("executor id");
        let paths = identity_paths(&root, &pool_id, &executor_id);
        load_or_create_identity(&paths).expect("identity exists");
        publish(&paths, &pool_id, &executor_id, "one");

        let reader_root = Arc::clone(&root);
        let reader_pool = pool_id.clone();
        let reader_executor = executor_id.clone();
        let reader = thread::spawn(move || {
            for _ in 0..100 {
                let context =
                    load_executor_connection(&reader_root, &reader_pool, &reader_executor)
                        .expect("published context loads");
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
            publish(&paths, &pool_id, &executor_id, generation);
        }
        reader.join().expect("reader exits");
    }

    fn publish(
        paths: &super::ExecutorIdentityPaths,
        pool_id: &BuildPoolId,
        executor_id: &BuildExecutorId,
        generation: &str,
    ) {
        publish_executor_context(
            paths,
            ExecutorContextPublication {
                organization_id: "org-1",
                pool_id,
                executor_id,
                nats_url: NatsClientUrl::try_new(format!("tls://{generation}.example:4222"))
                    .expect("NATS URL"),
                ca_pem: generation,
                credential_expires_at: 1,
            },
        )
        .expect("context publishes");
    }
}
