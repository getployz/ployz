use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_build_api::{BuildCommandPaths, BuildInvocationPlan};
use ployz_model::BuildMethod;
use sha2::{Digest, Sha256};

const BUILD_CACHE_KEY_RETRY_ATTEMPTS: usize = 20;
const BUILD_CACHE_KEY_RETRY_DELAY: Duration = Duration::from_millis(10);
const BUILD_CACHE_KEY_REPAIR_LOCK_STALE: Duration = Duration::from_secs(30);

struct CleanupDirsOnError {
    dirs: Vec<PathBuf>,
}

impl CleanupDirsOnError {
    fn new() -> Self {
        Self { dirs: Vec::new() }
    }

    fn push(&mut self, path: PathBuf) {
        self.dirs.push(path);
    }

    fn disarm(mut self) {
        self.dirs.clear();
    }
}

impl Drop for CleanupDirsOnError {
    fn drop(&mut self) {
        for path in &self.dirs {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub fn prepare_build_command_paths(
    data_dir: &Path,
    method: BuildMethod,
    operation_id: &str,
    invocation: &BuildInvocationPlan,
) -> Result<BuildCommandPaths, String> {
    let mut paths = BuildCommandPaths {
        cleanup_dirs: Vec::new(),
        railpack_plan_path: None,
        railpack_info_path: None,
        buildkit_secret_files: Vec::new(),
        railpack_secret_cache_token: None,
    };
    let mut cleanup_on_error = CleanupDirsOnError::new();
    if method == BuildMethod::Railpack || !invocation.secret_env.is_empty() {
        let metadata_dir = railpack_metadata_dir(data_dir, operation_id);
        create_private_dir(&metadata_dir)?;
        paths.cleanup_dirs.push(metadata_dir.clone());
        cleanup_on_error.push(metadata_dir.clone());
        if method == BuildMethod::Railpack {
            paths.railpack_plan_path = Some(metadata_dir.join("railpack-plan.json"));
            paths.railpack_info_path = Some(metadata_dir.join("railpack-info.json"));
        }
        if !invocation.secret_env.is_empty() {
            let secret_dir = metadata_dir.join("secrets");
            create_private_dir(&secret_dir)?;
            for (key, value) in &invocation.secret_env {
                let secret_path = secret_dir.join(key);
                write_private_file(&secret_path, value.as_bytes())?;
                paths.buildkit_secret_files.push((key.clone(), secret_path));
            }
        }
    }
    if invocation.railpack_secret_cache_required {
        let key = load_or_create_build_cache_key(data_dir)?;
        paths.railpack_secret_cache_token = Some(secret_cache_token(&key, &invocation.secret_env));
    }
    cleanup_on_error.disarm();
    Ok(paths)
}

fn railpack_metadata_dir(data_dir: &Path, operation_id: &str) -> PathBuf {
    data_dir.join("build-work").join(operation_id)
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|error| {
        format!(
            "create private build directory '{}': {error}",
            path.display()
        )
    })?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "set private build directory permissions '{}': {error}",
            path.display()
        )
    })?;
    Ok(())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    write_private_file_io(path, contents).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!("create private build secret '{}': {error}", path.display())
        } else {
            format!("write private build secret '{}': {error}", path.display())
        }
    })
}

fn write_private_file_io(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(contents)
}

fn load_or_create_build_cache_key(data_dir: &Path) -> Result<[u8; 32], String> {
    let key_dir = data_dir.join("build-work");
    create_private_dir(&key_dir)?;
    let key_path = key_dir.join("cache-token-key");
    let mut last_retryable_error = None;
    for _attempt in 0..BUILD_CACHE_KEY_RETRY_ATTEMPTS {
        match read_build_cache_key(&key_path) {
            Ok(key) => return Ok(key),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut key = [0_u8; 32];
                rand::fill(&mut key);
                match write_build_cache_key(&key_path, &key) {
                    Ok(()) => return Ok(key),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        last_retryable_error = Some(error);
                    }
                    Err(error) => {
                        return Err(format!(
                            "create build cache token key '{}': {error}",
                            key_path.display()
                        ));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                last_retryable_error = Some(error);
            }
            Err(error) => {
                return Err(format!(
                    "read build cache token key '{}': {error}",
                    key_path.display()
                ));
            }
        }
        std::thread::sleep(BUILD_CACHE_KEY_RETRY_DELAY);
    }
    let reason = last_retryable_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "retry attempts exhausted".into());
    match read_build_cache_key(&key_path) {
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return repair_build_cache_key(&key_path).map_err(|error| {
                format!(
                    "repair build cache token key '{}': {error}",
                    key_path.display()
                )
            });
        }
        Ok(key) => return Ok(key),
        Err(_error) => {}
    }
    Err(format!(
        "read build cache token key '{}' after {BUILD_CACHE_KEY_RETRY_ATTEMPTS} attempts: {reason}",
        key_path.display()
    ))
}

fn read_build_cache_key(key_path: &Path) -> std::io::Result<[u8; 32]> {
    let len = std::fs::metadata(key_path)?.len();
    if len != 32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("expected 32 byte build cache token key, found {len} bytes"),
        ));
    }
    let mut key = [0_u8; 32];
    let mut file = OpenOptions::new().read(true).open(key_path)?;
    file.read_exact(&mut key)?;
    Ok(key)
}

fn write_build_cache_key(key_path: &Path, key: &[u8; 32]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(key_path)?;
    file.write_all(key)
}

fn repair_build_cache_key(key_path: &Path) -> std::io::Result<[u8; 32]> {
    let lock_dir = key_path.with_file_name("cache-token-key.repair-lock");
    let mut last_retryable_error = None;
    for _attempt in 0..BUILD_CACHE_KEY_RETRY_ATTEMPTS {
        match try_acquire_repair_lock(&lock_dir)? {
            Some(_guard) => {
                return match read_build_cache_key(key_path) {
                    Ok(key) => Ok(key),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof
                        ) =>
                    {
                        replace_build_cache_key(key_path)
                    }
                    Err(error) => Err(error),
                };
            }
            None => match read_build_cache_key(key_path) {
                Ok(key) => return Ok(key),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof
                    ) =>
                {
                    last_retryable_error = Some(error);
                }
                Err(error) => return Err(error),
            },
        }
        std::thread::sleep(BUILD_CACHE_KEY_RETRY_DELAY);
    }
    match read_build_cache_key(key_path) {
        Ok(key) => Ok(key),
        Err(error) => Err(last_retryable_error.unwrap_or(error)),
    }
}

struct RepairLockGuard {
    path: PathBuf,
    token: String,
}

impl Drop for RepairLockGuard {
    fn drop(&mut self) {
        if repair_lock_token_matches(&self.path, &self.token).unwrap_or(false) {
            let _ = std::fs::remove_file(repair_lock_token_path(&self.path));
            let _ = std::fs::remove_dir(&self.path);
        }
    }
}

fn try_acquire_repair_lock(lock_dir: &Path) -> std::io::Result<Option<RepairLockGuard>> {
    let token = new_repair_lock_token();
    match std::fs::create_dir(lock_dir) {
        Ok(()) => {
            write_repair_lock_token(lock_dir, &token)?;
            Ok(Some(RepairLockGuard {
                path: lock_dir.to_path_buf(),
                token,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if repair_lock_is_stale(lock_dir)? {
                let observed_token = read_repair_lock_token(lock_dir)?;
                if repair_lock_is_stale(lock_dir)?
                    && repair_lock_observed_token_matches(lock_dir, observed_token.as_deref())?
                {
                    let _ = std::fs::remove_dir_all(lock_dir);
                }
                return match std::fs::create_dir(lock_dir) {
                    Ok(()) => {
                        write_repair_lock_token(lock_dir, &token)?;
                        Ok(Some(RepairLockGuard {
                            path: lock_dir.to_path_buf(),
                            token,
                        }))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
                    Err(error) => Err(error),
                };
            }
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn new_repair_lock_token() -> String {
    let mut bytes = [0_u8; 16];
    rand::fill(&mut bytes);
    hex_lower(&bytes)
}

fn write_repair_lock_token(lock_dir: &Path, token: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::set_permissions(lock_dir, std::fs::Permissions::from_mode(0o700))?;
    write_private_file_io(&repair_lock_token_path(lock_dir), token.as_bytes())
}

fn repair_lock_token_path(lock_dir: &Path) -> PathBuf {
    lock_dir.join("owner")
}

fn read_repair_lock_token(lock_dir: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(repair_lock_token_path(lock_dir)) {
        Ok(token) => Ok(Some(token)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn repair_lock_token_matches(lock_dir: &Path, token: &str) -> std::io::Result<bool> {
    Ok(read_repair_lock_token(lock_dir)?.as_deref() == Some(token))
}

fn repair_lock_observed_token_matches(
    lock_dir: &Path,
    observed_token: Option<&str>,
) -> std::io::Result<bool> {
    Ok(read_repair_lock_token(lock_dir)?.as_deref() == observed_token)
}

fn repair_lock_is_stale(lock_dir: &Path) -> std::io::Result<bool> {
    let modified = std::fs::metadata(lock_dir)?.modified()?;
    Ok(modified
        .elapsed()
        .is_ok_and(|age| age >= BUILD_CACHE_KEY_REPAIR_LOCK_STALE))
}

fn replace_build_cache_key(key_path: &Path) -> std::io::Result<[u8; 32]> {
    let mut key = [0_u8; 32];
    rand::fill(&mut key);
    let mut suffix = [0_u8; 8];
    rand::fill(&mut suffix);
    let temp_path =
        key_path.with_file_name(format!("cache-token-key.repair-{}", hex_lower(&suffix)));
    write_build_cache_key(&temp_path, &key)?;
    let result = replace_build_cache_key_from_temp(&temp_path, key_path);
    let _ = std::fs::remove_file(&temp_path);
    result.map(|()| key)
}

#[cfg(unix)]
fn replace_build_cache_key_from_temp(temp_path: &Path, key_path: &Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, key_path)
}

#[cfg(not(unix))]
fn replace_build_cache_key_from_temp(temp_path: &Path, key_path: &Path) -> std::io::Result<()> {
    let key = std::fs::read(temp_path)?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let mut file = options.open(key_path)?;
    file.write_all(&key)
}

fn secret_cache_token(key: &[u8; 32], secret_material: &[(String, String)]) -> String {
    let mut normalized_key = [0_u8; 64];
    for (target, source) in normalized_key.iter_mut().zip(key.iter()) {
        *target = *source;
    }
    let mut inner_key = [0_u8; 64];
    let mut outer_key = [0_u8; 64];
    for ((inner, outer), normalized) in inner_key
        .iter_mut()
        .zip(outer_key.iter_mut())
        .zip(normalized_key)
    {
        *inner = normalized ^ 0x36;
        *outer = normalized ^ 0x5c;
    }

    let mut hasher = Sha256::new();
    hasher.update(inner_key);
    hasher.update(b"ployz:railpack-secrets:v1\0");
    for (key, value) in secret_material {
        update_length_prefixed(&mut hasher, key.as_bytes());
        update_length_prefixed(&mut hasher, value.as_bytes());
    }
    let inner = hasher.finalize();

    let mut hasher = Sha256::new();
    hasher.update(outer_key);
    hasher.update(inner);
    hex_lower(&hasher.finalize())
}

fn update_length_prefixed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(value.len().to_be_bytes());
    hasher.update(value);
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(hex_nibble(byte >> 4));
        output.push(hex_nibble(byte & 0x0f));
    }
    output
}

fn hex_nibble(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        10..=15 => char::from(b'a' + (nibble - 10)),
        _ => '?',
    }
}
