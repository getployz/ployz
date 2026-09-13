//! Durable operator-local history. Independent computers and Cloud do not share
//! this lock and may still allocate overlapping subnets. No claims expire.
use ployz_core::{
    EnrollmentAssignment, EnrollmentSnapshot, MachineId, RegisterRequest, WireGuardPublicKey,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};

/// Failure to read or durably save operator-local enrollment history.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The history directory, lock, or assignment file could not be accessed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Saved history could not be decoded or encoded.
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    /// The request conflicts with history or cannot be allocated.
    #[error(transparent)]
    Allocation(#[from] ployz_core::EnrollmentError),
    /// No observed durable Machine identity connects the enrollment scope.
    #[error("enrollment scope requires an observed Entry Machine")]
    MissingScope,
}

#[derive(Default, Serialize, Deserialize)]
struct Scope {
    peers: Vec<(MachineId, WireGuardPublicKey)>,
    assignments: Vec<EnrollmentAssignment>,
}

/// Allocate and fsync history before returning, with no network operation under
/// the lock. Context names and addresses never identify a scope: observed durable
/// Machine identities connect aliases, including aliases using different peers.
/// Disjoint observations cannot establish that two entries share a Cluster.
///
/// # Errors
/// Returns storage failures, conflicting histories/requests, or pool exhaustion.
pub fn save_assignment(
    directory: &Path,
    request: &RegisterRequest,
    snapshot: &EnrollmentSnapshot,
) -> Result<EnrollmentAssignment, Error> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    // ponytail: one operator-wide lock; use per-scope locks if local enrollment throughput matters.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join("lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).map_err(io::Error::from)?;
    let path = directory.join("assignments.json");
    let mut scopes = load_scopes(&path)?;
    if snapshot.machines.is_empty() {
        return Err(Error::MissingScope);
    }
    let mut merged = Scope::default();
    for scope in std::mem::take(&mut scopes) {
        if snapshot
            .machines
            .iter()
            .any(|machine| scope.peers.contains(&(machine.id, machine.public_key)))
        {
            merged.peers.extend(scope.peers);
            merged.assignments.extend(scope.assignments);
        } else {
            scopes.push(scope);
        }
    }
    let assignment = ployz_core::allocate_enrollment(request, snapshot, &merged.assignments)?;
    if !merged
        .assignments
        .iter()
        .any(|saved| saved.machine.id == assignment.machine.id)
    {
        merged.assignments.push(assignment.clone());
    }
    // Reset preserves Machine identity. A new scope owns its witness, while the
    // previous scope keeps its allocation history for occupancy and explicit retries.
    for scope in &mut scopes {
        scope.peers.retain(|(id, _)| *id != assignment.machine.id);
    }
    for machine in snapshot
        .machines
        .iter()
        .chain(std::iter::once(&assignment.machine))
    {
        let peer = (machine.id, machine.public_key);
        if !merged.peers.contains(&peer) {
            merged.peers.push(peer);
        }
    }
    scopes.push(merged);
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec(&scopes)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    File::open(directory)?.sync_all()?;
    Ok(assignment)
}

fn load_scopes(path: &Path) -> Result<Vec<Scope>, Error> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

/// Check whether a joining Machine has saved work in this observed scope before
/// deciding whether a nonempty local lifecycle requires a destructive reset.
/// Allocation still validates all retry inputs under the lock.
///
/// # Errors
/// Returns unreadable or invalid history errors; never treats them as missing work.
pub fn has_assignment(
    directory: &Path,
    snapshot: &EnrollmentSnapshot,
    id: MachineId,
) -> Result<bool, Error> {
    Ok(load_scopes(&directory.join("assignments.json"))?
        .iter()
        .any(|scope| {
            snapshot
                .machines
                .iter()
                .any(|machine| scope.peers.contains(&(machine.id, machine.public_key)))
                && scope
                    .assignments
                    .iter()
                    .any(|assignment| assignment.machine.id == id)
        }))
}
