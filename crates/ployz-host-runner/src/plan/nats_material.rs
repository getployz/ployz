//! NATS material targets within Host Runner plans: the server config,
//! TLS material, authorized users, and client credential writes, plus the
//! listener/URL policy helpers they share.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use ployz_core::ids::MachineId;
use ployz_core::install::{NatsMachineMaterialPaths, WrappedCaKey, WrappedCoreSeeds};
use ployz_core::nats_config::{
    CredentialGrant, CredentialName, CredentialRole, NatsAuthorizationGrant, NatsCaCertificatePem,
    NatsInternalAuthority, NatsServerCertificatePem, NatsUserSeed,
};
use ployz_core::roles::DaemonProcessRole;
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::permissions::render_authorized_users;
use ployz_nats::server_config::{
    NatsAdvertisedHost, NatsListener, NatsServerConfig, NatsServerTlsFiles,
};

use crate::execution::NatsServerUnitTarget;
use crate::lifecycle::machine_join::{JOIN_NATS_CREDENTIALS_FILE, JOIN_TRUSTED_CA_FILE};
use crate::recovery::{ClusterNatsIdentity, NatsServerKeyPem};

pub(super) const DEFAULT_NATS_PORT: u16 = 4222;

/// The authorized-users include file name; NATS resolves the relative
/// include against the directory of `nats-server.conf`.
pub const AUTHORIZED_USERS_FILE_NAME: &str = "authorized-users.conf";

#[must_use]
pub(super) fn tls_loopback_nats_url(port: u16) -> NatsClientUrl {
    NatsClientUrl::try_new(format!("tls://127.0.0.1:{port}"))
        .expect("loopback TLS NATS URL is valid")
}

/// The listener flip: the NATS port becomes externally reachable only when
/// the install supplies a public address — in the same rendered config that
/// carries TLS and authorization.
#[must_use]
pub(super) fn first_machine_listener(machine_public_ip: Option<IpAddr>) -> NatsListener {
    match machine_public_ip {
        None => NatsListener::Loopback,
        Some(ip) => NatsListener::External {
            advertise_host: NatsAdvertisedHost::try_new(advertised_host_for_ip(ip))
                .expect("IP addresses render to valid advertised hosts"),
        },
    }
}

fn advertised_host_for_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

/// The NATS client credentials a daemon role's environment points at.
///
/// Every rendered role environment carries a CA file and a seed file — an
/// unauthenticated role environment is unrepresentable. Machine and gateway on
/// the first machine point at `machine.seed`, which does not exist at install
/// time; they await it with bounded retries (there is no controller-seed
/// fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleNatsCredentials {
    ca_file: PathBuf,
    seeds: RoleNatsSeedSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleNatsSeedSource {
    /// First-machine layout: per-principal seed files under the NATS state dir.
    ClusterMaterial(NatsMachineMaterialPaths),
    /// Joined-machine layout: every role authenticates with the single
    /// redeemed per-machine seed.
    SharedSeedFile(PathBuf),
}

impl RoleNatsCredentials {
    #[must_use]
    pub fn cluster(material: &NatsMachineMaterialPaths) -> Self {
        Self {
            ca_file: material.ca_file(),
            seeds: RoleNatsSeedSource::ClusterMaterial(material.clone()),
        }
    }

    /// Joined machines read the CA and per-machine seed committed by the
    /// Host Runner join into the join material directory.
    #[must_use]
    pub fn joined(join_material_dir: &Path) -> Self {
        Self {
            ca_file: join_material_dir.join(JOIN_TRUSTED_CA_FILE),
            seeds: RoleNatsSeedSource::SharedSeedFile(
                join_material_dir.join(JOIN_NATS_CREDENTIALS_FILE),
            ),
        }
    }

    #[must_use]
    pub fn ca_file(&self) -> &Path {
        &self.ca_file
    }

    #[must_use]
    pub fn seed_file_for_role(&self, role: &DaemonProcessRole) -> PathBuf {
        match &self.seeds {
            RoleNatsSeedSource::ClusterMaterial(material) => material.role_seed_file(role),
            RoleNatsSeedSource::SharedSeedFile(path) => path.clone(),
        }
    }

    #[must_use]
    pub fn join_seed_file(&self) -> Option<PathBuf> {
        match &self.seeds {
            RoleNatsSeedSource::ClusterMaterial(material) => Some(material.join_seed_file()),
            RoleNatsSeedSource::SharedSeedFile(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerConfigTarget {
    config_dir: PathBuf,
    config_file_name: String,
    rendered_config: String,
}

impl NatsServerConfigTarget {
    #[must_use]
    pub fn for_first_machine(
        machine_id: MachineId,
        unit: &NatsServerUnitTarget,
        material: &NatsMachineMaterialPaths,
        listener: NatsListener,
    ) -> Self {
        let config_path = unit.config_path().to_path_buf();
        let config_dir = config_path
            .parent()
            .expect("validated nats config path has a directory")
            .to_path_buf();
        let config_file_name = config_path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .expect("validated nats config path has a UTF-8 file name")
            .to_owned();

        Self {
            config_dir,
            config_file_name,
            rendered_config: NatsServerConfig::single_machine(
                machine_id,
                listener,
                NatsServerTlsFiles {
                    cert_file: material.server_cert_file(),
                    key_file: material.server_key_file(),
                },
                PathBuf::from(AUTHORIZED_USERS_FILE_NAME),
            )
            .expect("first-machine nats config is valid")
            .render(),
        }
    }

    #[must_use]
    pub fn display_path(&self) -> PathBuf {
        self.config_dir.join(&self.config_file_name)
    }

    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    #[must_use]
    pub fn config_file_name(&self) -> &str {
        &self.config_file_name
    }

    #[must_use]
    pub fn render_config(&self) -> String {
        self.rendered_config.clone()
    }
}

/// Writes `ca.pem`, `server.crt`, and `server.key` (key `0600`) into the
/// NATS state dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsTlsMaterialTarget {
    material: NatsMachineMaterialPaths,
    identity: ClusterNatsIdentity,
    recovery_key_wrapped: WrappedCaKey,
    core_seeds_wrapped: WrappedCoreSeeds,
}

impl NatsTlsMaterialTarget {
    #[must_use]
    pub fn new(
        material: NatsMachineMaterialPaths,
        identity: &ClusterNatsIdentity,
        recovery_key_wrapped: WrappedCaKey,
        core_seeds_wrapped: WrappedCoreSeeds,
    ) -> Self {
        Self {
            material,
            identity: identity.clone(),
            recovery_key_wrapped,
            core_seeds_wrapped,
        }
    }

    #[must_use]
    pub fn state_dir(&self) -> &Path {
        self.material.state_dir()
    }

    #[must_use]
    pub fn material(&self) -> &NatsMachineMaterialPaths {
        &self.material
    }

    #[must_use]
    pub fn ca_pem(&self) -> &NatsCaCertificatePem {
        &self.identity.ca
    }

    #[must_use]
    pub fn server_cert_pem(&self) -> &NatsServerCertificatePem {
        &self.identity.server_cert.cert_pem
    }

    #[must_use]
    pub fn server_key_pem(&self) -> &NatsServerKeyPem {
        &self.identity.server_cert.key_pem
    }

    #[must_use]
    pub fn recovery_key_wrapped(&self) -> &WrappedCaKey {
        &self.recovery_key_wrapped
    }

    #[must_use]
    pub fn core_seeds_wrapped(&self) -> &WrappedCoreSeeds {
        &self.core_seeds_wrapped
    }
}

/// Writes the initial `authorized-users.conf` next to the server config.
///
/// Host Runner writes this file exactly once at install; `ployzd` control owns
/// every later rewrite (machine-add minting, machine-remove).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsAuthorizedUsersTarget {
    config_dir: PathBuf,
    file_name: String,
    rendered: String,
}

/// The new core's own control-plane principals, the base of every rendered
/// authorized-users set (first-machine install and promotion alike).
fn core_principal_users(
    identity: &ClusterNatsIdentity,
    founder_credential_name: &CredentialName,
) -> Vec<NatsAuthorizationGrant> {
    vec![
        NatsAuthorizationGrant::Internal {
            authority: NatsInternalAuthority::Controller,
            public_key: identity.controller.public.clone(),
        },
        NatsAuthorizationGrant::Credential(CredentialGrant {
            public_key: identity.operator.public.clone(),
            name: founder_credential_name.clone(),
            role: CredentialRole::Operator,
        }),
        NatsAuthorizationGrant::Internal {
            authority: NatsInternalAuthority::Join,
            public_key: identity.join.public.clone(),
        },
    ]
}

impl NatsAuthorizedUsersTarget {
    /// The install-time user set: Controller, operator Operator, and Join.
    /// Machine users are minted later by `ployzd` control.
    #[must_use]
    pub fn initial_for_first_machine(
        config_dir: PathBuf,
        identity: &ClusterNatsIdentity,
        founder_credential_name: &CredentialName,
        additional_credentials: &[CredentialGrant],
    ) -> Self {
        let mut users = core_principal_users(identity, founder_credential_name);
        users.extend(
            additional_credentials
                .iter()
                .cloned()
                .map(NatsAuthorizationGrant::Credential),
        );
        Self {
            config_dir,
            file_name: AUTHORIZED_USERS_FILE_NAME.to_owned(),
            rendered: render_authorized_users(&users),
        }
    }

    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    #[must_use]
    pub fn display_path(&self) -> PathBuf {
        self.config_dir.join(&self.file_name)
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.rendered.clone()
    }
}

/// Writes `controller.seed`, `operator.seed`, and `join.seed` (`0600`)
/// into the NATS state dir. `machine.seed` is deliberately absent: `ployzd`
/// control writes it at activate-first-machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsClientCredentialsTarget {
    material: NatsMachineMaterialPaths,
    identity: ClusterNatsIdentity,
}

impl NatsClientCredentialsTarget {
    #[must_use]
    pub fn new(material: NatsMachineMaterialPaths, identity: &ClusterNatsIdentity) -> Self {
        Self {
            material,
            identity: identity.clone(),
        }
    }

    #[must_use]
    pub fn state_dir(&self) -> &Path {
        self.material.state_dir()
    }

    #[must_use]
    pub fn material(&self) -> &NatsMachineMaterialPaths {
        &self.material
    }

    #[must_use]
    pub fn controller_seed(&self) -> &NatsUserSeed {
        &self.identity.controller.seed
    }

    #[must_use]
    pub fn operator_seed(&self) -> &NatsUserSeed {
        &self.identity.operator.seed
    }

    #[must_use]
    pub fn join_seed(&self) -> &NatsUserSeed {
        &self.identity.join.seed
    }
}
