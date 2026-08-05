//! Shared operator-context selection and persistent mesh dialing.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use ployz_core::ids::ClusterId;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::commands::SshTarget;
use crate::init::ssh::default_config_home;
use crate::mesh::context::{
    LoadedOperatorContext, OperatorContextError, OperatorContextStore, UnsupportedMeshProvider,
};
use crate::mesh::http::{JsonReply, MeshApiClient, MeshApiClientError};
use crate::mesh::{
    BuiltinWireguardDial, BuiltinWireguardPeer, MeshConnectError, MeshConnector, MeshDialTimeouts,
    MeshStream,
};

const API_PORT: u16 = 2_020;

#[derive(Debug, Clone)]
pub struct OperatorRemote {
    connector: MeshConnector,
    api_target: SocketAddr,
    cluster_id: ClusterId,
}

impl OperatorRemote {
    pub fn load(target: Option<&SshTarget>) -> Result<Self, OperatorRemoteError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(OperatorRemoteError::MissingHome)?;
        let store = OperatorContextStore::new(default_config_home(&home));
        let loaded = load_context(&store, target)?;
        Self::try_from_loaded(loaded)
    }

    fn try_from_loaded(loaded: LoadedOperatorContext) -> Result<Self, OperatorRemoteError> {
        match loaded {
            LoadedOperatorContext::BuiltinWireguard(context) => {
                let dial = BuiltinWireguardDial::new(
                    context.private_key.bytes(),
                    context.source_address,
                    BuiltinWireguardPeer {
                        public_key: context.machine_public_key,
                        endpoint: context.machine_endpoint,
                        allowed_subnet: context.machine_allowed_subnet,
                    },
                    context.target.as_str(),
                );
                Ok(Self {
                    connector: MeshConnector::builtin_wireguard(dial, MeshDialTimeouts::default()),
                    api_target: SocketAddr::new(IpAddr::V6(context.machine_address), API_PORT),
                    cluster_id: context.cluster_id,
                })
            }
            LoadedOperatorContext::UnsupportedProvider(context) => {
                Err(OperatorRemoteError::UnsupportedProvider {
                    provider: context.provider,
                })
            }
        }
    }

    pub async fn connect(&self) -> Result<MeshStream, OperatorRemoteError> {
        self.connector
            .connect(self.api_target)
            .await
            .map_err(OperatorRemoteError::Connect)
    }

    pub async fn request_json<RequestBody, ResponseBody>(
        &self,
        method: hyper::Method,
        route: &str,
        body: Option<&RequestBody>,
    ) -> Result<ResponseBody, OperatorRemoteError>
    where
        RequestBody: Serialize + ?Sized,
        ResponseBody: DeserializeOwned,
    {
        let stream = self.connect().await?;
        MeshApiClient::default()
            .request_json(stream, method, route, body)
            .await
            .map_err(OperatorRemoteError::Api)
    }

    pub async fn request_json_with_refusal<RequestBody, ResponseBody, Refusal>(
        &self,
        method: hyper::Method,
        route: &str,
        body: Option<&RequestBody>,
    ) -> Result<JsonReply<ResponseBody, Refusal>, OperatorRemoteError>
    where
        RequestBody: Serialize + ?Sized,
        ResponseBody: DeserializeOwned,
        Refusal: DeserializeOwned,
    {
        let stream = self.connect().await?;
        MeshApiClient::default()
            .request_json_with_refusal(stream, method, route, body)
            .await
            .map_err(OperatorRemoteError::Api)
    }

    pub async fn lens(
        &self,
        collection: ployz_core::LensCollection,
    ) -> Result<ployz_core::LensSnapshot, OperatorRemoteError> {
        let stream = self.connect().await?;
        MeshApiClient::default()
            .lens(stream, collection)
            .await
            .map_err(OperatorRemoteError::Api)
    }

    #[must_use]
    pub const fn cluster_id(&self) -> &ClusterId {
        &self.cluster_id
    }
}

fn load_context(
    store: &OperatorContextStore,
    requested: Option<&SshTarget>,
) -> Result<LoadedOperatorContext, OperatorRemoteError> {
    if let Some(target) = requested {
        return match store.load_target(target) {
            Ok(context) => Ok(context),
            Err(OperatorContextError::MissingContext { .. }) => {
                Err(ContextSelectionError::UnconfiguredTarget {
                    target: target.clone(),
                }
                .into())
            }
            Err(error) => Err(error.into()),
        };
    }

    let contexts = store.load_all()?;
    let targets = contexts
        .iter()
        .map(|loaded| loaded.target().clone())
        .collect::<Vec<_>>();
    let selected = select_context_index(&targets, None)?;
    let Some(loaded) = contexts.into_iter().nth(selected) else {
        unreachable!("selected operator context index comes from the same collection")
    };
    Ok(loaded)
}

#[derive(Debug, thiserror::Error)]
pub enum OperatorRemoteError {
    #[error("HOME is not set; cannot load operator contexts")]
    MissingHome,
    #[error(transparent)]
    Context(#[from] OperatorContextError),
    #[error(transparent)]
    Selection(#[from] ContextSelectionError),
    #[error(
        "mesh provider {provider:?} is not shipped; builtin WireGuard is the only supported provider"
    )]
    UnsupportedProvider { provider: UnsupportedMeshProvider },
    #[error(transparent)]
    Connect(#[from] MeshConnectError),
    #[error(transparent)]
    Api(#[from] MeshApiClientError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextSelectionError {
    NotConfigured,
    UnconfiguredTarget {
        target: SshTarget,
    },
    Ambiguous {
        targets: Vec<String>,
    },
    UnknownTarget {
        target: String,
        configured: Vec<String>,
    },
}

impl fmt::Display for ContextSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => formatter
                .write_str("no operator context is configured; run `ployz init root@<host>` first"),
            Self::UnconfiguredTarget { target } => write!(
                formatter,
                "no operator context is configured for {target}; run `ployz init {target}` first"
            ),
            Self::Ambiguous { targets } => {
                formatter.write_str("multiple operator contexts are configured; choose one:")?;
                for target in targets {
                    write!(formatter, "\n  {target}: add `--target {target}`")?;
                }
                Ok(())
            }
            Self::UnknownTarget { target, configured } => {
                write!(
                    formatter,
                    "no operator context is configured for {target}; configured targets:"
                )?;
                for target in configured {
                    write!(formatter, "\n  {target}: add `--target {target}`")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ContextSelectionError {}

pub(crate) fn select_context_index(
    configured: &[SshTarget],
    requested: Option<&SshTarget>,
) -> Result<usize, ContextSelectionError> {
    match requested {
        Some(requested) => configured
            .iter()
            .position(|candidate| candidate == requested)
            .ok_or_else(|| ContextSelectionError::UnknownTarget {
                target: requested.as_str().to_owned(),
                configured: sorted_target_names(configured),
            }),
        None => match configured {
            [] => Err(ContextSelectionError::NotConfigured),
            [_] => Ok(0),
            [_, _, ..] => Err(ContextSelectionError::Ambiguous {
                targets: sorted_target_names(configured),
            }),
        },
    }
}

fn sorted_target_names(targets: &[SshTarget]) -> Vec<String> {
    let mut targets = targets
        .iter()
        .map(|target| target.as_str().to_owned())
        .collect::<Vec<_>>();
    targets.sort();
    targets
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ployz_core::corrosion::{MachineTransport, MeshProvider, derive_builtin_wireguard_member};
    use ployz_core::network::{MachineEndpointSubnet, WireGuardPublicKey};

    use super::*;
    use crate::mesh::context::{SshContextHandoff, SshPeerKey, context_path, key_path};

    fn target(value: &str) -> SshTarget {
        value.parse().expect("valid SSH target")
    }

    fn persist_context(store: &OperatorContextStore, home: &std::path::Path, target: &SshTarget) {
        let key = SshPeerKey::generate("laptop".to_owned()).expect("peer key");
        key.persist_new(home, target).expect("persist peer key");
        let cluster_id = ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id");
        let machine_key =
            WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("machine key");
        store
            .persist(
                target,
                SshContextHandoff {
                    machine_transport: MachineTransport::Wireguard {
                        addr_v6: derive_builtin_wireguard_member(&cluster_id, &machine_key)
                            .bind_address()
                            .get(),
                        pubkey: machine_key,
                        endpoint: Some("203.0.113.7:51820".parse().expect("endpoint")),
                        subnet_v4: MachineEndpointSubnet::try_new("10.210.0.0/24")
                            .expect("machine subnet"),
                    },
                    cluster_id,
                    provider: MeshProvider::BuiltinWireguard,
                },
                &key,
            )
            .expect("persist context");
    }

    #[test]
    fn context_selection_is_shared_copy_for_every_remote_command() {
        assert_eq!(
            select_context_index(&[], None).expect_err("no context"),
            ContextSelectionError::NotConfigured
        );
        assert_eq!(
            ContextSelectionError::NotConfigured.to_string(),
            "no operator context is configured; run `ployz init root@<host>` first"
        );

        let one = vec![target("root@one.example")];
        assert_eq!(select_context_index(&one, None).expect("one context"), 0);

        let multiple = vec![target("root@z.example"), target("root@a.example")];
        assert_eq!(
            select_context_index(&multiple, None)
                .expect_err("selector required")
                .to_string(),
            "multiple operator contexts are configured; choose one:\n  root@a.example: add `--target root@a.example`\n  root@z.example: add `--target root@z.example`"
        );
        assert_eq!(
            select_context_index(&multiple, Some(&target("root@z.example"))).expect("exact target"),
            0
        );
        assert_eq!(
            select_context_index(&multiple, Some(&target("root@missing.example")))
                .expect_err("unknown target")
                .to_string(),
            "no operator context is configured for root@missing.example; configured targets:\n  root@a.example: add `--target root@a.example`\n  root@z.example: add `--target root@z.example`"
        );
    }

    #[test]
    fn explicit_target_does_not_load_an_unrelated_invalid_context() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = OperatorContextStore::new(temporary.path());
        let good = target("root@good.example");
        persist_context(&store, temporary.path(), &good);

        let bad = target("root@bad.example");
        fs::write(context_path(temporary.path(), &bad), b"{")
            .expect("write invalid unrelated context");

        let loaded = load_context(&store, Some(&good)).expect("load requested context");
        assert_eq!(loaded.target(), &good);
        assert!(matches!(
            store.load_all(),
            Err(OperatorContextError::InvalidDocument(_))
        ));
    }

    #[test]
    fn missing_explicit_target_names_the_init_command_without_scanning() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = OperatorContextStore::new(temporary.path());
        let requested = target("root@missing.example");

        let error = load_context(&store, Some(&requested)).expect_err("missing target");
        assert!(matches!(
            &error,
            OperatorRemoteError::Selection(ContextSelectionError::UnconfiguredTarget {
                target,
            }) if target.as_str() == "root@missing.example"
        ));
        assert_eq!(
            error.to_string(),
            "no operator context is configured for root@missing.example; run `ployz init root@missing.example` first"
        );
    }

    #[test]
    fn missing_peer_key_remains_a_context_filesystem_error() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = OperatorContextStore::new(temporary.path());
        let requested = target("root@incomplete.example");
        persist_context(&store, temporary.path(), &requested);
        fs::remove_file(key_path(temporary.path(), &requested)).expect("remove peer key");

        let error = load_context(&store, Some(&requested)).expect_err("missing peer key");
        assert!(matches!(
            error,
            OperatorRemoteError::Context(OperatorContextError::Filesystem(error))
                if error.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[test]
    fn unsupported_provider_refuses_before_dialing() {
        let loaded = LoadedOperatorContext::UnsupportedProvider(
            crate::mesh::context::UnsupportedOperatorContext {
                target: target("root@machine.example"),
                cluster_id: ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id"),
                provider: UnsupportedMeshProvider::Tailscale,
            },
        );

        let error = OperatorRemote::try_from_loaded(loaded).expect_err("unsupported provider");
        assert!(matches!(
            error,
            OperatorRemoteError::UnsupportedProvider {
                provider: UnsupportedMeshProvider::Tailscale
            }
        ));
    }
}
